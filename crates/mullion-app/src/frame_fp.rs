//! F159:整帧指纹 —— 「这一帧画出来跟上一帧一模一样吗」。
//!
//! **零 GPU、零 IO**,可纯单测:吃的是 egui 已经 tessellate 出来的顶点、
//! 终端各 pane 的行指纹与几何,吐一个 `u64`。
//!
//! 为什么判在**结果**上而不是判在**原因**上:见
//! [ADR-011](../../../docs/adr-011-row-fingerprint-vs-term-damage.md)
//! (F12 的行指纹为什么不用 `Term::damage()`)。同一条推理 —— 能改变
//! 「这一帧长什么样」的来源列举不完,漏一个的症状是屏幕留着陈旧的一帧,
//! 编译/测试/日志全静默,只有人眼能发现。**失败方向也跟着反过来**:
//! 指纹的最坏情况是多画一帧,枚举式判据的最坏情况是少画。

use mullion_term::snapshot::{Cursor, Rgb};

use crate::gpu::{style_for, PaneRender};
use crate::shell::workspace::{PaneGeom, PxRect};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// 增量式 FNV-1a。与 `mullion_term::snapshot::hash_row` 同一套常数
/// (那边算「一行长什么样」,这边算「一帧长什么样」)。
#[derive(Debug, Clone, Copy)]
struct Fnv(u64);

impl Fnv {
    const fn new() -> Self {
        Self(FNV_OFFSET)
    }
    fn byte(&mut self, b: u8) {
        self.0 ^= b as u64;
        self.0 = self.0.wrapping_mul(FNV_PRIME);
    }
    fn u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.byte(b);
        }
    }
    fn u32(&mut self, v: u32) {
        self.u64(v as u64);
    }
    /// **位模式**,不是数值。`f32` 绝不用 `==` 比,也绝不进 `derive(PartialEq)`。
    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    fn bool(&mut self, v: bool) {
        self.byte(v as u8);
    }
    /// 先吃长度再吃内容 —— 不吃长度的话 `("ab","c")` 与 `("a","bc")` 同哈希。
    fn bytes(&mut self, s: &[u8]) {
        self.u64(s.len() as u64);
        for &b in s {
            self.byte(b);
        }
    }
    fn rgb(&mut self, c: Rgb) {
        let Rgb { r, g, b } = c;
        self.byte(r);
        self.byte(g);
        self.byte(b);
    }
}

/// 一帧的指纹。
///
/// **刻意不 `derive(PartialEq)`**:`Unhashable == Unhashable` 会被 derive
/// 判成 `true`,而那正好是「含 paint callback 的两帧被判成一样、于是永远
/// 不再重画」这个静默故障。比较一律走 [`FrameFp::same_as`]。
#[derive(Debug, Clone, Copy)]
pub enum FrameFp {
    Hash(u64),
    /// 本帧含无法指纹化的内容(egui 的 paint callback)。与任何东西都不相同,
    /// **包括另一个 `Unhashable`** —— 回调每帧可以画出不同的东西,我们看不见。
    /// 保守方向:多画一帧,永不少画。
    Unhashable,
}

impl FrameFp {
    /// 两帧是否**确定**一模一样。
    pub fn same_as(&self, other: &FrameFp) -> bool {
        match (self, other) {
            (FrameFp::Hash(a), FrameFp::Hash(b)) => a == b,
            _ => false,
        }
    }
}

/// 影响文字层最终长相、但不进任何行指纹的样式量(F21 字体族/字号、
/// F80 兜底文字色、DPI)。
///
/// **刻意不 `derive(PartialEq)`**:里面有 `f32`,比较一律走位模式。
#[derive(Debug, Clone, Copy)]
pub struct StyleKey<'a> {
    pub family: &'a str,
    pub font_px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub default_fg: Rgb,
}

/// 这一帧能不能跳过 GPU 提交。
///
/// `deltas_empty`:egui 这一帧有没有待上传 / 待释放的纹理增量。**非空一律
/// 判 miss** —— 那两份 delta 是 egui 每帧 drain 出来、**只交付一次**的
/// (字体图集的新字形栅格 / 纹理回收),跳掉就永久丢了,之后某帧会引用
/// 一张从未上传的纹理:花屏或 panic,且只在「先命中、后未命中」的序列里
/// 发作,无头测试完全够不到。真实频率极低,不影响收益。
pub fn can_skip(prev: Option<&FrameFp>, cur: &FrameFp, deltas_empty: bool) -> bool {
    deltas_empty && prev.is_some_and(|p| p.same_as(cur))
}

/// 算这一帧的整帧指纹。
///
/// `surface` 是交换链的 `(width, height)` —— 窗口尺寸变了必须重画,而
/// 尺寸不进 egui 顶点也不进行指纹。
pub fn frame_fingerprint(
    paint_jobs: &[egui::ClippedPrimitive],
    panes: &[PaneRender<'_>],
    blink_on: bool,
    style: StyleKey<'_>,
    surface: (u32, u32),
) -> FrameFp {
    let mut h = Fnv::new();
    h.u32(surface.0);
    h.u32(surface.1);
    hash_style(&mut h, style);
    if !hash_paint_jobs(&mut h, paint_jobs) {
        return FrameFp::Unhashable;
    }
    h.u64(panes.len() as u64);
    for p in panes {
        hash_pane(&mut h, p, blink_on);
    }
    FrameFp::Hash(h.0)
}

/// 样式量单独取指纹(F172 行带差分用)。
///
/// **刻意复用 `hash_style`** 而不是另写一遍:那个函数是穷尽解构的,
/// `StyleKey` 加字段时它编译报错。复制一份的话新字段只会漏进行带指纹里
/// —— 换字体后某些带留着旧字号的字,而**编译、测试、日志全静默**。
pub fn style_digest(style: StyleKey<'_>) -> u64 {
    let mut h = Fnv::new();
    hash_style(&mut h, style);
    h.0
}

fn hash_style(h: &mut Fnv, style: StyleKey<'_>) {
    // 穷尽解构 —— 加字段时这里编译报错,强迫作者对「进不进指纹」表态。
    let StyleKey {
        family,
        font_px,
        cell_w,
        cell_h,
        default_fg,
    } = style;
    h.bytes(family.as_bytes());
    h.f32(font_px);
    h.f32(cell_w);
    h.f32(cell_h);
    h.rgb(default_fg);
}

/// 返回 `false` = 本帧含 paint callback,指纹不成立。
fn hash_paint_jobs(h: &mut Fnv, jobs: &[egui::ClippedPrimitive]) -> bool {
    h.u64(jobs.len() as u64);
    for job in jobs {
        // 穷尽解构 —— 同上。
        let egui::ClippedPrimitive {
            clip_rect,
            primitive,
        } = job;
        h.f32(clip_rect.min.x);
        h.f32(clip_rect.min.y);
        h.f32(clip_rect.max.x);
        h.f32(clip_rect.max.y);
        match primitive {
            egui::epaint::Primitive::Mesh(mesh) => {
                let egui::epaint::Mesh {
                    indices,
                    vertices,
                    texture_id,
                } = mesh;
                match texture_id {
                    egui::TextureId::Managed(id) => {
                        h.byte(0);
                        h.u64(*id);
                    }
                    egui::TextureId::User(id) => {
                        h.byte(1);
                        h.u64(*id);
                    }
                }
                h.u64(indices.len() as u64);
                for i in indices {
                    h.u32(*i);
                }
                h.u64(vertices.len() as u64);
                for v in vertices {
                    let egui::epaint::Vertex { pos, uv, color } = *v;
                    h.f32(pos.x);
                    h.f32(pos.y);
                    h.f32(uv.x);
                    h.f32(uv.y);
                    for c in color.to_array() {
                        h.byte(c);
                    }
                }
            }
            // 回调每帧可以画出不同的东西,我们看不见 —— 一律判「变了」。
            // 我们目前不用 paint callback,但这条分支必须写:将来有人加了
            // 之后静默失效(屏幕停在旧的一帧)是不可接受的。
            egui::epaint::Primitive::Callback(_) => return false,
        }
    }
    true
}

fn hash_pane(h: &mut Fnv, pane: &PaneRender<'_>, blink_on: bool) {
    // 穷尽解构 —— 给 `PaneRender` 加字段时编译报错。
    let PaneRender {
        geom,
        snap,
        focused,
        preedit,
    } = pane;
    hash_geom(h, geom);
    h.bool(*focused);
    // F126 组字串。**这一分量不能省**:preedit 画在终端文字层(复用
    // `SnapCell::width` 的宽度判据),不在 egui 的 paint_jobs 里;而组字过程
    // 中 cells 不变、行指纹不变 —— 漏掉它,指纹在整个组字过程中恒命中,
    // **打拼音屏幕纹丝不动**,正是 T10 那一族「只有人眼能发现」的坑。
    h.bytes(preedit.as_bytes());
    h.u32(snap.cols as u32);
    h.u32(snap.rows as u32);
    let Cursor {
        row,
        col,
        visible,
        shape,
        blinking,
    } = snap.cursor;
    h.u32(row as u32);
    h.u32(col as u32);
    h.bool(visible);
    h.bool(blinking);
    // F125 闪烁相位:吃 `style_for` 的**结果**而不是裸的 `blink_on`。
    // 吃裸值的话,一块 pane 都没有的 launcher 态也会跟着相位每秒变 2 次
    // 指纹(白出 2 帧),而非焦点 pane 恒画空心光标、根本不跟着闪,也会
    // 被算成「变了」。判在结果上,这两件事自动对。
    h.byte(style_for(shape, *focused, blink_on) as u8);
    // F12 的行指纹。`SnapCell.selected` 在内,划选反色自动覆盖;主题换色
    // 也已经烘进快照的 fg/bg。
    for r in 0..snap.rows {
        h.u64(snap.row_hash(r));
    }
}

fn hash_geom(h: &mut Fnv, g: &PaneGeom) {
    // 穷尽解构 —— 同上。
    let PaneGeom {
        id,
        px,
        title_px,
        term_px,
        grid,
    } = *g;
    h.u32(id.0);
    for r in [px, title_px, term_px] {
        let PxRect { x, y, w, h: rect_h } = r;
        h.u32(x);
        h.u32(y);
        h.u32(w);
        h.u32(rect_h);
    }
    h.u32(grid.0 as u32);
    h.u32(grid.1 as u32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_core::layout::PaneId;
    use mullion_term::snapshot::{CursorShape, GridSnapshot, SnapCell};

    fn cell(ch: char) -> SnapCell {
        SnapCell {
            ch,
            fg: Rgb {
                r: 200,
                g: 200,
                b: 200,
            },
            bg: Rgb { r: 0, g: 0, b: 0 },
            width: 1,
            spacer: false,
            selected: false,
        }
    }

    fn snap_of(text: &str) -> GridSnapshot {
        let cells: Vec<SnapCell> = text.chars().map(cell).collect();
        GridSnapshot::new(
            cells.len() as u16,
            1,
            cells,
            Cursor {
                row: 0,
                col: 0,
                visible: true,
                shape: CursorShape::Beam,
                blinking: true,
            },
        )
    }

    fn geom() -> PaneGeom {
        PaneGeom {
            id: PaneId(1),
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
            grid: (80, 24),
        }
    }

    fn style() -> StyleKey<'static> {
        StyleKey {
            family: "Google Sans Code",
            font_px: 16.0,
            cell_w: 9.0,
            cell_h: 20.0,
            default_fg: Rgb {
                r: 200,
                g: 200,
                b: 200,
            },
        }
    }

    fn mesh_job(x: f32) -> egui::ClippedPrimitive {
        let mut mesh = egui::epaint::Mesh::default();
        mesh.indices.push(0);
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(x, 0.0),
            uv: egui::pos2(0.0, 0.0),
            color: egui::Color32::WHITE,
        });
        egui::ClippedPrimitive {
            clip_rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0)),
            primitive: egui::epaint::Primitive::Mesh(mesh),
        }
    }

    /// 一模一样的两帧必须判「一样」。这是整个 F159 收益的前提 ——
    /// 它红了说明指纹里混进了每帧都变的东西(时间戳、地址、迭代顺序),
    /// 症状是画面完全正确、CPU 一点没降,只有 `fp=hit:0/miss:N` 看得出来。
    #[test]
    fn two_identical_frames_hash_the_same() {
        let s = snap_of("hello");
        let panes = [PaneRender {
            geom: geom(),
            snap: &s,
            focused: true,
            preedit: "",
        }];
        let jobs = [mesh_job(1.0)];
        let a = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        let b = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        assert!(a.same_as(&b), "同样的输入算出了不同的指纹");
    }

    /// 终端内容变了必须判「变了」。
    ///
    /// 自证会变红:把 `hash_pane` 里那个 `row_hash` 循环删掉。
    #[test]
    fn a_changed_row_changes_the_fingerprint() {
        let a = snap_of("hello");
        let b = snap_of("hellp");
        let jobs = [mesh_job(1.0)];
        let fa = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: geom(),
                snap: &a,
                focused: true,
                preedit: "",
            }],
            true,
            style(),
            (800, 600),
        );
        let fb = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: geom(),
                snap: &b,
                focused: true,
                preedit: "",
            }],
            true,
            style(),
            (800, 600),
        );
        assert!(
            !fa.same_as(&fb),
            "改了一个字,指纹却没变 —— 屏幕会留着陈旧的一行"
        );
    }

    /// 划选反色必须判「变了」。
    ///
    /// `SnapCell.selected` 已经在 F12 的行指纹里,这条钉的是「那条链路
    /// 真的接到了整帧指纹上」——断了的症状是拖鼠标划选时选区完全不显示。
    ///
    /// 自证会变红:同上(删 `row_hash` 循环)。
    #[test]
    fn selecting_text_changes_the_fingerprint() {
        let plain = snap_of("hello");
        let mut selected_cells: Vec<SnapCell> = "hello".chars().map(cell).collect();
        selected_cells[0].selected = true;
        let selected = GridSnapshot::new(5, 1, selected_cells, plain.cursor);
        let jobs = [mesh_job(1.0)];
        let fa = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: geom(),
                snap: &plain,
                focused: true,
                preedit: "",
            }],
            true,
            style(),
            (800, 600),
        );
        let fb = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: geom(),
                snap: &selected,
                focused: true,
                preedit: "",
            }],
            true,
            style(),
            (800, 600),
        );
        assert!(!fa.same_as(&fb), "划选没改变指纹 —— 选区永远画不出来");
    }

    /// **只改组字串**必须判「变了」(T10 一族)。
    ///
    /// preedit 画在终端文字层(F126),不在 egui 的 paint_jobs 里;而组字
    /// 过程中 cells 不变、行指纹不变 —— 漏掉这一分量的症状是
    /// **打拼音屏幕纹丝不动**,编译/测试/日志全静默。
    ///
    /// 自证会变红:把 `hash_pane` 里那句 `h.bytes(preedit.as_bytes());` 删掉。
    #[test]
    fn typing_pinyin_changes_the_fingerprint_even_though_the_cells_do_not() {
        let s = snap_of("hello");
        let jobs = [mesh_job(1.0)];
        let fa = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: geom(),
                snap: &s,
                focused: true,
                preedit: "ni",
            }],
            true,
            style(),
            (800, 600),
        );
        let fb = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: geom(),
                snap: &s,
                focused: true,
                preedit: "nih",
            }],
            true,
            style(),
            (800, 600),
        );
        assert!(
            !fa.same_as(&fb),
            "组字串变了指纹没变 —— 打拼音时屏幕会纹丝不动"
        );
    }

    /// 焦点 pane 的光标闪烁相位翻转必须判「变了」——否则光标不闪。
    /// 非焦点 pane 恒画空心光标,相位翻转对它是**不变**的,不该白出一帧。
    ///
    /// 自证会变红:把 `hash_pane` 里的 `style_for(shape, *focused, blink_on)`
    /// 换成裸的 `blink_on`(非焦点那条会红),或者整句删掉(焦点那条会红)。
    #[test]
    fn only_the_focused_pane_churns_when_the_blink_phase_flips() {
        let s = snap_of("hello");
        let jobs = [mesh_job(1.0)];
        let fp = |focused: bool, blink: bool| {
            frame_fingerprint(
                &jobs,
                &[PaneRender {
                    geom: geom(),
                    snap: &s,
                    focused,
                    preedit: "",
                }],
                blink,
                style(),
                (800, 600),
            )
        };
        assert!(
            !fp(true, true).same_as(&fp(true, false)),
            "焦点 pane 的相位翻转没改变指纹 —— 光标不会闪"
        );
        assert!(
            fp(false, true).same_as(&fp(false, false)),
            "非焦点 pane 恒画空心光标,却跟着相位白出帧"
        );
    }

    /// **一块 pane 都没有**(launcher 态)时,相位翻转不该改变指纹。
    ///
    /// 这条直接对应人工验收清单第 1 条:launcher 静置 `present` 要接近 0。
    /// 吃裸 `blink_on` 的话,launcher 会稳定地每秒出 2 帧。
    ///
    /// 自证会变红:在 `frame_fingerprint` 里加一句 `h.bool(blink_on);`。
    #[test]
    fn the_launcher_does_not_churn_with_the_cursor_blink() {
        let jobs = [mesh_job(1.0)];
        let a = frame_fingerprint(&jobs, &[], true, style(), (800, 600));
        let b = frame_fingerprint(&jobs, &[], false, style(), (800, 600));
        assert!(
            a.same_as(&b),
            "launcher 里没有光标,却跟着闪烁相位每秒白出 2 帧"
        );
    }

    /// egui 侧动一个顶点就必须判「变了」——菜单高亮、tooltip、动画全靠它。
    ///
    /// 自证会变红:把 `hash_paint_jobs` 里的 `vertices` 循环删掉。
    #[test]
    fn moving_an_egui_vertex_changes_the_fingerprint() {
        let s = snap_of("hello");
        let panes = [PaneRender {
            geom: geom(),
            snap: &s,
            focused: true,
            preedit: "",
        }];
        let a = frame_fingerprint(&[mesh_job(1.0)], &panes, true, style(), (800, 600));
        let b = frame_fingerprint(&[mesh_job(2.0)], &panes, true, style(), (800, 600));
        assert!(
            !a.same_as(&b),
            "egui 顶点动了指纹没变 —— 菜单/悬停反馈会卡住"
        );
    }

    /// paint callback **永远**不判命中,包括跟另一个 callback 帧比。
    ///
    /// 我们目前不用 paint callback。这条钉的是「将来有人加了之后不会静默
    /// 失效」——`derive(PartialEq)` 会让 `Unhashable == Unhashable` 成立,
    /// 那正好是「屏幕永久停在加 callback 那一帧」。
    ///
    /// 自证会变红:给 `FrameFp` 加 `derive(PartialEq)` 并把 `same_as` 改成 `self == other`。
    #[test]
    fn a_paint_callback_frame_is_never_considered_unchanged() {
        assert!(!FrameFp::Unhashable.same_as(&FrameFp::Unhashable));
        assert!(!FrameFp::Unhashable.same_as(&FrameFp::Hash(7)));
        assert!(!FrameFp::Hash(7).same_as(&FrameFp::Unhashable));
        assert!(FrameFp::Hash(7).same_as(&FrameFp::Hash(7)));
    }

    /// 换字体族 / 字号 / DPI 必须判「变了」。
    ///
    /// 这几样一个都不进行指纹(行指纹只认内容和颜色),漏掉的症状是
    /// **换完字体屏幕停在旧字体的那一帧上**。
    ///
    /// 自证会变红:把 `frame_fingerprint` 里的 `hash_style` 调用删掉。
    #[test]
    fn changing_the_font_changes_the_fingerprint() {
        let s = snap_of("hello");
        let panes = [PaneRender {
            geom: geom(),
            snap: &s,
            focused: true,
            preedit: "",
        }];
        let jobs = [mesh_job(1.0)];
        let base = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        let bigger = StyleKey {
            font_px: 18.0,
            ..style()
        };
        let other_family = StyleKey {
            family: "Consolas",
            ..style()
        };
        assert!(
            !base.same_as(&frame_fingerprint(&jobs, &panes, true, bigger, (800, 600))),
            "字号变了指纹没变"
        );
        assert!(
            !base.same_as(&frame_fingerprint(
                &jobs,
                &panes,
                true,
                other_family,
                (800, 600)
            )),
            "字体族变了指纹没变"
        );
    }

    /// 窗口尺寸变了必须判「变了」。尺寸不进 egui 顶点也不进行指纹。
    ///
    /// 自证会变红:把 `frame_fingerprint` 里那两句 `h.u32(surface.*)` 删掉。
    #[test]
    fn resizing_the_surface_changes_the_fingerprint() {
        let s = snap_of("hello");
        let panes = [PaneRender {
            geom: geom(),
            snap: &s,
            focused: true,
            preedit: "",
        }];
        let jobs = [mesh_job(1.0)];
        let a = frame_fingerprint(&jobs, &panes, true, style(), (800, 600));
        let b = frame_fingerprint(&jobs, &panes, true, style(), (801, 600));
        assert!(!a.same_as(&b), "窗口尺寸变了指纹没变");
    }

    /// 拖动分屏分界线(几何变了、内容没变)必须判「变了」。
    ///
    /// 自证会变红:把 `hash_pane` 里的 `hash_geom` 调用删掉。
    #[test]
    fn dragging_a_split_changes_the_fingerprint() {
        let s = snap_of("hello");
        let jobs = [mesh_job(1.0)];
        let wide = geom();
        let narrow = PaneGeom {
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 400,
                h: 600,
            },
            ..geom()
        };
        let a = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: wide,
                snap: &s,
                focused: true,
                preedit: "",
            }],
            true,
            style(),
            (800, 600),
        );
        let b = frame_fingerprint(
            &jobs,
            &[PaneRender {
                geom: narrow,
                snap: &s,
                focused: true,
                preedit: "",
            }],
            true,
            style(),
            (800, 600),
        );
        assert!(!a.same_as(&b), "pane 几何变了指纹没变 —— 拖分界线画面不跟");
    }

    /// 两块 pane 内容对调必须判「变了」——关掉中间一块时其后所有 pane
    /// 会整体挪位,判成「没变」就是张冠李戴,屏幕上两块 pane 的内容互换。
    ///
    /// 自证会变红:把 `frame_fingerprint` 里对 `panes` 的 `for` 改成先按
    /// 某种规范序排序再哈希。
    #[test]
    fn swapping_two_panes_changes_the_fingerprint() {
        let a = snap_of("aaa");
        let b = snap_of("bbb");
        let g2 = PaneGeom {
            id: PaneId(2),
            ..geom()
        };
        let jobs = [mesh_job(1.0)];
        let ab = frame_fingerprint(
            &jobs,
            &[
                PaneRender {
                    geom: geom(),
                    snap: &a,
                    focused: true,
                    preedit: "",
                },
                PaneRender {
                    geom: g2,
                    snap: &b,
                    focused: false,
                    preedit: "",
                },
            ],
            true,
            style(),
            (800, 600),
        );
        let ba = frame_fingerprint(
            &jobs,
            &[
                PaneRender {
                    geom: geom(),
                    snap: &b,
                    focused: true,
                    preedit: "",
                },
                PaneRender {
                    geom: g2,
                    snap: &a,
                    focused: false,
                    preedit: "",
                },
            ],
            true,
            style(),
            (800, 600),
        );
        assert!(!ab.same_as(&ba), "两块 pane 的内容对调却判成没变");
    }

    /// `textures_delta` 非空的帧**一律**判 miss。
    ///
    /// 那两份 delta 是 egui 每帧 drain 出来、只交付一次的(字体图集的新
    /// 字形栅格 / 纹理回收)。指纹命中就跳的话它被静默丢弃,之后某帧引用
    /// 一张从未上传的纹理 —— 花屏或 panic,且只在「先命中、后未命中」的
    /// 序列里发作,无头测试完全够不到。
    ///
    /// 自证会变红:把 `can_skip` 里的 `deltas_empty &&` 去掉。
    #[test]
    fn a_pending_texture_delta_forces_a_miss() {
        let fp = FrameFp::Hash(7);
        assert!(can_skip(Some(&FrameFp::Hash(7)), &fp, true));
        assert!(
            !can_skip(Some(&FrameFp::Hash(7)), &fp, false),
            "有待交付的纹理增量却跳了帧 —— 增量会被静默丢弃"
        );
    }

    /// 第一帧没有可比对的上一帧,必须画。
    ///
    /// 自证会变红:把 `can_skip` 里的 `prev.is_some_and(..)` 改成
    /// `prev.map_or(true, ..)`。
    #[test]
    fn the_first_frame_is_never_a_hit() {
        assert!(!can_skip(None, &FrameFp::Hash(7), true));
    }

    /// `GridSnapshot` 加了新字段却没进整帧指纹,症状是屏幕留着陈旧的一帧。
    ///
    /// 那个结构体有私有字段,crate 外**无法穷尽解构**,所以只能靠扫源码。
    /// 字段覆盖面本身由 `mullion-term` 那边 `hash_row` 的穷尽解构 + 六条
    /// 逐字段测试守着;这条只负责在「结构体长出新字段」时把人拦下来。
    ///
    /// 自证会变红:往 `GridSnapshot` 里加一个 `pub display_offset: usize,`。
    #[test]
    fn the_snapshot_has_not_grown_a_field_behind_the_fingerprints_back() {
        let src = include_str!("../../mullion-term/src/snapshot.rs");
        let body = src
            .split("pub struct GridSnapshot {")
            .nth(1)
            .expect("找不到 GridSnapshot 的定义 —— 切片失效,断言会恒绿")
            .split("\n}")
            .next()
            .expect("GridSnapshot 的定义没有结尾");
        let fields: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| l.ends_with(',') && !l.starts_with("///") && !l.starts_with("//"))
            .collect();
        assert_eq!(
            fields,
            vec![
                "pub cols: u16,",
                "pub rows: u16,",
                "pub cells: Vec<SnapCell>,",
                "pub cursor: Cursor,",
                "row_hash: Vec<u64>,",
            ],
            "GridSnapshot 的字段变了 —— 回 `frame_fp::hash_pane` 决定新字段进不进整帧指纹"
        );
    }
}
