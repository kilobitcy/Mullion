//! winit 键盘事件 → term keymap 的 (Key, Mods)。纯映射,可脱离窗口单测。
//! 编码本身(含 T6 Shift+Enter)在 `mullion_term::keymap::encode_key`,这里只做翻译。

use std::time::Instant;

use mullion_term::keymap::{Key, Mods};
use mullion_term::selection::{CellSide, SelectionKind};
use winit::event::{KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

/// 把一次 winit 按键事件翻译成 term 的 (Key, Mods);无法映射的键返回 None。
///
/// `KeyEvent::platform_specific` 字段对外部 crate 是 `pub(crate)`,测试里无法
/// 直接构造 `KeyEvent`,因此可测逻辑抽到 [`translate_logical`],这里只做转调。
pub fn translate_key(event: &KeyEvent, mods: ModifiersState) -> Option<(Key, Mods)> {
    translate_logical(&event.logical_key, mods)
}

/// 纯翻译逻辑:接收 `logical_key` 与修饰键状态,返回 term 的 (Key, Mods)。
pub fn translate_logical(logical: &WKey, mods: ModifiersState) -> Option<(Key, Mods)> {
    let m = Mods {
        shift: mods.shift_key(),
        ctrl: mods.control_key(),
        alt: mods.alt_key(),
        sup: mods.super_key(),
    };
    let key = match logical {
        WKey::Named(NamedKey::Enter) => Key::Enter,
        // 空格/常用控制键都作为 NamedKey 送达,不走 Character——早期漏映射导致
        // 空格等「很多键没反应」。
        WKey::Named(NamedKey::Space) => Key::Space,
        WKey::Named(NamedKey::Tab) => Key::Tab,
        WKey::Named(NamedKey::Backspace) => Key::Backspace,
        WKey::Named(NamedKey::Escape) => Key::Escape,
        WKey::Named(NamedKey::Delete) => Key::Delete,
        WKey::Named(NamedKey::ArrowUp) => Key::Up,
        WKey::Named(NamedKey::ArrowDown) => Key::Down,
        WKey::Named(NamedKey::ArrowLeft) => Key::Left,
        WKey::Named(NamedKey::ArrowRight) => Key::Right,
        WKey::Named(NamedKey::PageUp) => Key::PageUp,
        WKey::Named(NamedKey::PageDown) => Key::PageDown,
        WKey::Character(s) => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // 多字符(IME 合成)MVP 先不处理
            }
            Key::Char(c)
        }
        _ => return None,
    };
    Some((key, m))
}

/// 一次滚轮增量 → 行数(正数 = 向上 / 往历史)。
///
/// `LineDelta` 一格按 3 行(与主流终端一致)。`PixelDelta`(触控板/精密滚轮)按
/// 行高换算,**不足一行也至少给 ±1**——直接截断的话触控板小幅滚动永远无反应。
pub fn wheel_lines(delta: MouseScrollDelta, cell_h: f32) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => (y * 3.0).round() as i32,
        MouseScrollDelta::PixelDelta(p) => {
            let h = if cell_h > 0.0 { cell_h } else { 1.0 };
            let raw = p.y as f32 / h;
            let n = raw.trunc() as i32;
            if n != 0 {
                n
            } else if raw > 0.0 {
                1
            } else if raw < 0.0 {
                -1
            } else {
                0
            }
        }
    }
}

/// 指针物理像素坐标 → 1-based 终端单元格 `(col, row)`,夹紧在 `dims` 内。
///
/// **收的是「终端区局部坐标」,不是窗口坐标**:终端自绘层整体平移到了 egui
/// 中央区(菜单栏之下),所以调用方必须先减去中央区原点。换算只在
/// `App::cursor_in_grid` 一处做——两边原点不同源,上报的行号就整体偏移。
pub fn cell_at(px: (f32, f32), cell: (f32, f32), dims: (u16, u16)) -> (u16, u16) {
    let cw = if cell.0 > 0.0 { cell.0 } else { 1.0 };
    let ch = if cell.1 > 0.0 { cell.1 } else { 1.0 };
    let col = (px.0 / cw).floor().max(0.0) as u32 + 1;
    let row = (px.1 / ch).floor().max(0.0) as u32 + 1;
    (
        col.min(dims.0.max(1) as u32) as u16,
        row.min(dims.1.max(1) as u32) as u16,
    )
}

/// 连击时间窗(ms)。Windows 双击间隔默认 500ms,取同量级——太长会把两次
/// 不相干的单击粘成双击,太短则连击选词经常判不出来。
const MULTI_CLICK_MS: u128 = 400;
/// 连击的位置容差(单元格)。手在按键瞬间会抖 1 格,不给容差双击很难触发。
const MULTI_CLICK_SLOP: u16 = 1;
/// 自动滚动每帧上限(行)。不封顶的话把指针甩到屏幕外一帧就冲到 scrollback
/// 顶端,选区直接失控。
const AUTOSCROLL_MAX_LINES: i32 = 5;

/// 一次左键按下的连击状态。由 [`click_kind`] 产出,调用方存着下次传回来。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrevClick {
    pub at: Instant,
    pub pos: (u16, u16),
    /// 连击序号:1 = 单击,2 = 双击,3 = 三击(第 4 击回到 1)。
    pub count: u8,
}

/// 指针在单元格内落在左半还是右半(F18)。
///
/// 决定该格算不算进选区,直接影响"跟手"——只按格号取整的话,选区边界会
/// 比视觉滞后半格。
///
/// `cols` 是网格列数,用来把越界指针夹进网格:拖拽时指针会移出窗口,
/// [`cell_at`] 已经把列号夹到首/末格,半格判定必须**同源**地夹,否则指针在
/// 窗外继续移动时列号不动、半格标志却在翻转,选区边界会抖
/// (见 `cell_side_clamps_out_of_bounds_pointer_like_cell_at`)。
///
/// 格内分数用 `floor` 而不是 `f32::fract()`:后者是向零截断,负数区间与
/// [`cell_at`] 的 `floor` 取整不是同一套规则。
pub fn cell_side(px_x: f32, cell_w: f32, cols: u16) -> CellSide {
    // cell_w 为 0(字体测量失败)时除零得 NaN,而 NaN 的比较恒 false,
    // 会一路判成 Right —— 选区整体偏一格。兜底成 1.0。
    let w = if cell_w > 0.0 { cell_w } else { 1.0 };
    let q = px_x / w;
    let cell = q.floor();
    if cell < 0.0 {
        CellSide::Left
    } else if cell > cols.saturating_sub(1) as f32 {
        CellSide::Right
    } else if q - cell < 0.5 {
        CellSide::Left
    } else {
        CellSide::Right
    }
}

/// 判定本次左键按下是单击 / 双击 / 三击,并给出更新后的连击状态。
///
/// winit 不提供连击判定,得自己做。`now` 作为参数传入而不是函数内取当前时间,
/// 否则没法测。第 4 击回到单击,与主流终端一致。
pub fn click_kind(
    prev: Option<PrevClick>,
    now: Instant,
    pos: (u16, u16),
) -> (SelectionKind, PrevClick) {
    let count = match prev {
        Some(p)
            if now.duration_since(p.at).as_millis() <= MULTI_CLICK_MS
                && p.pos.0.abs_diff(pos.0) <= MULTI_CLICK_SLOP
                && p.pos.1.abs_diff(pos.1) <= MULTI_CLICK_SLOP =>
        {
            if p.count >= 3 {
                1
            } else {
                p.count + 1
            }
        }
        _ => 1,
    };
    let kind = match count {
        2 => SelectionKind::Semantic,
        3 => SelectionKind::Lines,
        _ => SelectionKind::Simple,
    };
    (
        kind,
        PrevClick {
            at: now,
            pos,
            count,
        },
    )
}

/// 拖拽时指针越出窗口上/下边界要滚几行(F18:选区跨多屏 scrollback)。
///
/// 正数 = 往历史(向上),与 `mullion_term::emulator::Emulator::scroll` 的
/// `Scroll::Delta` 语义一致。边界内返回 0。越界越远滚越快,封顶
/// [`AUTOSCROLL_MAX_LINES`]。
pub fn autoscroll_lines(px_y: f32, win_h: f32, cell_h: f32) -> i32 {
    // 窗口最小化时 win_h 可能是 0,那时任何正的 px_y 都会落进「越出下边界」
    // 分支、无端往下滚。最小化本就不该有划选,直接不滚。
    if win_h <= 0.0 {
        return 0;
    }
    let h = if cell_h > 0.0 { cell_h } else { 1.0 };
    if px_y < 0.0 {
        (((-px_y) / h).ceil() as i32).clamp(1, AUTOSCROLL_MAX_LINES)
    } else if px_y > win_h {
        -((((px_y - win_h) / h).ceil() as i32).clamp(1, AUTOSCROLL_MAX_LINES))
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;

    #[test]
    fn enter_maps_to_key_enter() {
        let (key, mods) =
            translate_logical(&WKey::Named(NamedKey::Enter), ModifiersState::SHIFT).unwrap();
        assert_eq!(key, Key::Enter);
        assert!(mods.shift);
    }

    #[test]
    fn char_maps_to_key_char() {
        let (key, _) =
            translate_logical(&WKey::Character("a".into()), ModifiersState::empty()).unwrap();
        assert_eq!(key, Key::Char('a'));
    }

    #[test]
    fn multichar_ime_is_ignored() {
        // 多字符(输入法合成)MVP 先不当按键处理,交给后续 IME 支持。
        assert!(
            translate_logical(&WKey::Character("ab".into()), ModifiersState::empty()).is_none()
        );
    }

    #[test]
    fn space_named_key_maps_to_space() {
        // 回归:空格是 NamedKey::Space,不是 Character(" ")。早期漏了这条 → 空格没反应。
        let (key, _) =
            translate_logical(&WKey::Named(NamedKey::Space), ModifiersState::empty()).unwrap();
        assert_eq!(key, Key::Space);
    }

    #[test]
    fn common_named_keys_are_mapped() {
        let e = ModifiersState::empty();
        let m = |n| translate_logical(&WKey::Named(n), e).map(|(k, _)| k);
        assert_eq!(m(NamedKey::Tab), Some(Key::Tab));
        assert_eq!(m(NamedKey::Backspace), Some(Key::Backspace));
        assert_eq!(m(NamedKey::Escape), Some(Key::Escape));
        assert_eq!(m(NamedKey::Delete), Some(Key::Delete));
        assert_eq!(m(NamedKey::ArrowUp), Some(Key::Up));
        assert_eq!(m(NamedKey::ArrowDown), Some(Key::Down));
        assert_eq!(m(NamedKey::ArrowLeft), Some(Key::Left));
        assert_eq!(m(NamedKey::ArrowRight), Some(Key::Right));
        assert_eq!(m(NamedKey::PageUp), Some(Key::PageUp));
        assert_eq!(m(NamedKey::PageDown), Some(Key::PageDown));
    }

    #[test]
    fn line_delta_is_three_lines_per_notch() {
        assert_eq!(wheel_lines(MouseScrollDelta::LineDelta(0.0, 1.0), 16.0), 3);
        assert_eq!(
            wheel_lines(MouseScrollDelta::LineDelta(0.0, -2.0), 16.0),
            -6
        );
    }

    #[test]
    fn small_pixel_delta_still_scrolls_at_least_one_line() {
        // 触控板一次只送几个像素;截断成 0 的话触控板永远滚不动。
        let tiny = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 3.0));
        assert_eq!(wheel_lines(tiny, 16.0), 1);
        let tiny_down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -3.0));
        assert_eq!(wheel_lines(tiny_down, 16.0), -1);
        // 大增量按行高换算。
        let big = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 48.0));
        assert_eq!(wheel_lines(big, 16.0), 3);
        // 镜像:大幅负增量同样按行高换算,符号不能翻——方向搞反是这类代码
        // 最常见的 bug,且在无头环境里靠人眼滚一下才能发现,必须靠测试钉住。
        let big_down = MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, -48.0));
        assert_eq!(wheel_lines(big_down, 16.0), -3);
    }

    #[test]
    fn cell_at_is_one_based_and_clamped() {
        // 鼠标上报的坐标是 1-based,且必须夹在网格内——越界坐标会让对端 TUI 误判。
        assert_eq!(cell_at((0.0, 0.0), (8.0, 16.0), (80, 24)), (1, 1));
        assert_eq!(cell_at((23.0, 33.0), (8.0, 16.0), (80, 24)), (3, 3));
        assert_eq!(
            cell_at((10_000.0, 10_000.0), (8.0, 16.0), (80, 24)),
            (80, 24)
        );
        assert_eq!(cell_at((-5.0, -5.0), (8.0, 16.0), (80, 24)), (1, 1));
    }

    #[test]
    fn cell_side_splits_at_half_cell() {
        // 落在格左半 → 该格不算进选区;右半 → 算进去。半格判定直接影响"跟手"。
        assert_eq!(cell_side(0.0, 8.0, 80), CellSide::Left);
        assert_eq!(cell_side(3.9, 8.0, 80), CellSide::Left);
        assert_eq!(cell_side(4.0, 8.0, 80), CellSide::Right);
        assert_eq!(cell_side(7.9, 8.0, 80), CellSide::Right);
        // 下一格重新从左半开始。
        assert_eq!(cell_side(8.0, 8.0, 80), CellSide::Left);
    }

    #[test]
    fn cell_side_survives_zero_cell_width() {
        // cell_w 在字体测量失败时可能是 0;除零会得到 NaN,NaN 比较恒 false,
        // 结果是永远判 Right —— 选区整体偏一格。这里兜底成 Left。
        assert_eq!(cell_side(3.0, 0.0, 80), CellSide::Left);
    }

    #[test]
    fn cell_side_clamps_out_of_bounds_pointer_like_cell_at() {
        // 拖拽时指针会移出窗口(winit 给负坐标 / 超窗口宽的坐标)。cell_at 会把
        // 列号夹到首/末格,半格判定必须同源地夹——否则列号不动、半格标志却随
        // 指针继续移动而翻转,选区边界在窗外抖。
        // 曾经的实现用 f32::fract()(向零截断),负数区间给出的是:
        //   -1.0→Left  -3.0→Left  -4.0→Right  -5.0→Right  -9.0→Left  -13.0→Right
        // 非单调,正是抖动的来源。
        for px in [-1.0, -3.0, -4.0, -5.0, -9.0, -13.0, -1000.0] {
            assert_eq!(
                cell_side(px, 8.0, 80),
                CellSide::Left,
                "px={px} 窗口左外侧应恒为 Left"
            );
        }
        // 右侧越界:列号夹在末格,半格应恒为 Right,否则拖到右边界外选不到行尾。
        for px in [640.0, 700.0, 10_000.0] {
            assert_eq!(
                cell_side(px, 8.0, 80),
                CellSide::Right,
                "px={px} 窗口右外侧应恒为 Right"
            );
        }
        // 末格内部仍正常二分(80 列 × 8px:第 79 格是 632.0..640.0)。
        assert_eq!(cell_side(632.0, 8.0, 80), CellSide::Left);
        assert_eq!(cell_side(636.0, 8.0, 80), CellSide::Right);
    }

    #[test]
    fn double_and_triple_click_are_detected_then_wrap() {
        // winit 不提供连击判定,自己做。第 4 击回到单击(与主流终端一致)。
        let t0 = Instant::now();
        let (k1, p1) = click_kind(None, t0, (5, 5));
        assert_eq!(k1, SelectionKind::Simple);
        let (k2, p2) = click_kind(Some(p1), t0 + Duration::from_millis(100), (5, 5));
        assert_eq!(k2, SelectionKind::Semantic);
        let (k3, p3) = click_kind(Some(p2), t0 + Duration::from_millis(200), (5, 5));
        assert_eq!(k3, SelectionKind::Lines);
        let (k4, _) = click_kind(Some(p3), t0 + Duration::from_millis(300), (5, 5));
        assert_eq!(k4, SelectionKind::Simple, "第 4 击应回到单击");
    }

    #[test]
    fn slow_second_click_is_a_fresh_single_click() {
        let t0 = Instant::now();
        let (_, p1) = click_kind(None, t0, (5, 5));
        let (k2, _) = click_kind(Some(p1), t0 + Duration::from_millis(5_000), (5, 5));
        assert_eq!(k2, SelectionKind::Simple, "超时后不该判成双击");
    }

    #[test]
    fn click_far_away_restarts_the_count() {
        // 位置容差 1 格:手抖挪一格仍算连击,挪远了就是新的一次单击——
        // 否则在文档里点两个不相干的位置会莫名其妙选中一个词。
        let t0 = Instant::now();
        let (_, p1) = click_kind(None, t0, (5, 5));
        let (k_near, _) = click_kind(Some(p1), t0 + Duration::from_millis(100), (6, 5));
        assert_eq!(k_near, SelectionKind::Semantic, "漂移 1 格仍算连击");
        let (k_far, _) = click_kind(Some(p1), t0 + Duration::from_millis(100), (20, 5));
        assert_eq!(k_far, SelectionKind::Simple, "漂移超过 1 格应重新计数");
    }

    #[test]
    fn autoscroll_is_zero_inside_the_window() {
        assert_eq!(autoscroll_lines(0.0, 480.0, 16.0), 0);
        assert_eq!(autoscroll_lines(240.0, 480.0, 16.0), 0);
        assert_eq!(autoscroll_lines(480.0, 480.0, 16.0), 0);
    }

    #[test]
    fn autoscroll_direction_matches_emulator_scroll_semantics() {
        // Emulator::scroll(Scroll::Delta(正数)) = 往历史(向上)。拖出上边界要看
        // 更旧的内容 → 正数;拖出下边界 → 负数。符号搞反在无头环境只能靠测试钉住。
        assert!(autoscroll_lines(-1.0, 480.0, 16.0) > 0);
        assert!(autoscroll_lines(481.0, 480.0, 16.0) < 0);
    }

    #[test]
    fn autoscroll_speeds_up_with_distance_but_is_capped() {
        // 越界越多滚越快,但要封顶:不封的话把指针甩到屏幕外一帧就冲到
        // scrollback 顶端,选区直接失控。
        let near = autoscroll_lines(-16.0, 480.0, 16.0);
        let far = autoscroll_lines(-160.0, 480.0, 16.0);
        assert!(far > near, "越界越远应滚得越快");
        assert_eq!(
            autoscroll_lines(-10_000.0, 480.0, 16.0),
            5,
            "必须封顶在 5 行"
        );
    }

    #[test]
    fn autoscroll_is_zero_when_window_or_cell_is_degenerate() {
        // 窗口最小化(win_h = 0)时不该滚:否则任何正的 px_y 都被当成越出下边界。
        assert_eq!(autoscroll_lines(100.0, 0.0, 16.0), 0);
        // cell_h = 0(字体测量失败)走 1.0 兜底,不能除零得 NaN 后 as i32 变成 0 或乱数。
        assert_eq!(autoscroll_lines(-3.0, 480.0, 0.0), 3);
        assert_eq!(autoscroll_lines(240.0, 480.0, 0.0), 0);
    }
}
