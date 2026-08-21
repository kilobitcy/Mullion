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
                return None; // 多字符(合成输入)不是「一个键」,归 `translate_text`
            }
            Key::Char(c)
        }
        _ => return None,
    };
    Some((key, m))
}

/// 无法映射成单个键、但仍是可打印文本的按键 → 原样发给远端的文本。
///
/// 合成输入(死键、部分键盘布局)会把结果整段塞进 `Character`。旧实现在
/// [`translate_logical`] 里直接丢弃,症状是"按出来的字凭空消失"。
///
/// 三条边界:
/// - **单字符不走这里**:那条路要经 `encode_key`,T6 的 Shift+Enter 等编码规则
///   都在那儿,被文本路径抢走就全退化成裸字符。
/// - **Ctrl/Alt/Super 不走这里**:那是快捷键,当文本发过去是往远端灌乱码。
///   Shift 例外——Shift+字母本来就是大写字母,是正经文本。
/// - 真正的中文/日文输入走的是 `WindowEvent::Ime`(见 [`ImeState`]),不是这条。
pub fn translate_text(logical: &WKey, mods: ModifiersState) -> Option<String> {
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return None;
    }
    let WKey::Character(s) = logical else {
        return None;
    };
    s.chars().nth(1)?; // 单字符归 translate_logical
    Some(s.to_string())
}

/// IME 组字状态(F21 输入法)。
///
/// **为什么必须有**:winit 在组字期间照样发 `KeyboardInput`,`logical_key` 就是
/// 用户敲的拼音字母。不拦的话打「你好」会先往远端送一串 `nihao`,再送「你好」。
///
/// 三条结束边:`Commit`(选了字)、空 `Preedit`(按 Esc 取消候选)、`Disabled`
/// (切走输入法 / 失焦)。**少认一条,组字状态就永久挂着,此后一个键都进不了
/// 终端** —— 与 T8 同一类"输入永久失灵"的故障。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImeState {
    /// F126:组字中的拼音串,要画在光标处。**这是唯一真值源** —— 「在不在组字」
    /// 就是「这串空不空」,不另留一个 bool:两份状态迟早会在某条结束边上失步,
    /// 而失步的那一半正好是「永久吞键」这种最难查的故障。
    text: String,
}

impl ImeState {
    /// 收到 `Ime::Preedit`。空串 = 候选被取消,组字结束。
    pub fn on_preedit(&mut self, text: &str) {
        // 复用已分配的堆缓冲:组字期间每敲一个字母就来一次,`= text.to_owned()`
        // 会一路分配再释放。
        self.text.clear();
        self.text.push_str(text);
    }

    /// 收到 `Ime::Commit`,组字结束。
    pub fn on_commit(&mut self) {
        self.text.clear();
    }

    /// 收到 `Ime::Disabled`(切走输入法 / 失焦),组字结束。
    pub fn on_disabled(&mut self) {
        self.text.clear();
    }

    /// 这一刻的按键该不该被吞掉(组字中 = 该吞)。
    pub fn swallows_key(&self) -> bool {
        !self.text.is_empty()
    }

    /// F126:组字中的拼音串,空串 = 没在组字。
    pub fn preedit(&self) -> &str {
        &self.text
    }
}

/// IME 提交的文本 → 发给远端的字节。
///
/// 换行归一成 `\r`,与 `mullion_term::keymap::encode_paste` 同一套规则:
/// 少数输入法会一次提交带换行的整段,送 `\n` 过去 shell 不换行。
/// 空提交返回 `None` —— 取消候选时某些平台会补一条空 commit。
///
/// **不做 bracketed paste 包裹**:这是逐字输入,不是粘贴;包起来会让远端把
/// 用户敲的每个字当成一次粘贴事件(shell 的括号粘贴提示会一路刷屏)。
pub fn ime_commit_bytes(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }
    let out: String = text.replace("\r\n", "\r").replace('\n', "\r");
    Some(out.into_bytes())
}

/// F149:这一帧该往 `egui_winit::State` 的 IME 账本里写什么。`None` = 别动它。
///
/// **窗口的 IME 归宿主所有,egui 不许关它。** egui-winit 的去抖是
/// 「账本 ≠ 目标值才发 `set_ime_allowed`」(`lib.rs:849`)。egui 里没有文本框
/// 在组字的帧,目标值是 `false`;把账本预先写成同一个 `false`,那次调用就
/// 发不出去,窗口保持 `resumed` 里设的常开。
///
/// 终端不是 egui 部件,egui 永远不会知道它也需要 IME —— 不按住账本的话,
/// 用户点过一次任意输入框(换节点搜索框、路径条、标签改名、会话管理器字段)
/// 再点回终端,中文输入就永久没了,且**没有自愈路径**,只能重启。
///
/// 返回 `Some(true)` 是**反的**:那会制造 `true != false`,禁用调用每帧必发。
/// 这是复核阶段真的写反过一次的地方,`the_ime_ledger_is_clamped_to_false_...`
/// 钉着方向。
pub fn ime_ledger_clamp(egui_wants_ime: bool) -> Option<bool> {
    if egui_wants_ime {
        None
    } else {
        Some(false)
    }
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

    /// F149:egui 不要 IME 的那些帧,账本必须被写成 **false**。
    ///
    /// 写 `true` 是反的 —— egui 的去抖是「账本 ≠ 目标值才发调用」,目标值这时
    /// 正是 `false`,写 true 反而每帧都触发一次 `set_ime_allowed(false)`,
    /// 窗口从第一帧起就没有 IME。这条断言钉的就是这个方向。
    #[test]
    fn the_ime_ledger_is_clamped_to_false_so_egui_never_disables_the_window_ime() {
        assert_eq!(
            ime_ledger_clamp(false),
            Some(false),
            "egui 不要 IME 时,账本要写成与目标值相同的 false,去抖才会短路"
        );
    }

    /// egui 自己要 IME 的帧不许动账本:它这时要发的是 `set_ime_allowed(true)`,
    /// 对一个本就开着的窗口无害,插手只会让两边的账再次对不上。
    #[test]
    fn the_ime_ledger_is_left_alone_while_egui_is_composing() {
        assert_eq!(ime_ledger_clamp(true), None);
    }

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
    fn multichar_is_not_a_single_key() {
        // 多字符(输入法合成 / 死键组合)不是「一个键」,`translate_logical` 交给
        // `translate_text` 走文本路径,不在这里硬塞进 `Key::Char`。
        assert!(
            translate_logical(&WKey::Character("ab".into()), ModifiersState::empty()).is_none()
        );
    }

    #[test]
    fn multichar_character_is_sent_as_text_so_composed_input_reaches_the_remote() {
        // F21/输入法:部分布局与死键组合会把合成结果整段塞进 `Character`。
        // 旧实现直接 `return None` 丢掉 —— 用户按出来的字凭空消失。
        assert_eq!(
            translate_text(&WKey::Character("ǹǐ".into()), ModifiersState::empty()).as_deref(),
            Some("ǹǐ")
        );
    }

    #[test]
    fn single_char_is_not_taken_by_the_text_path() {
        // 单字符走 `translate_logical` → `encode_key`(那里才有 T6 的 Shift+Enter
        // 等编码规则)。文本路径抢走的话所有修饰键组合都会退化成裸字符。
        assert_eq!(
            translate_text(&WKey::Character("a".into()), ModifiersState::empty()),
            None
        );
    }

    #[test]
    fn multichar_with_ctrl_or_alt_is_a_shortcut_not_text() {
        // 带 Ctrl/Alt/Super 的组合是快捷键,原样当文本发过去就是往远端灌乱码。
        for m in [
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SUPER,
        ] {
            assert_eq!(
                translate_text(&WKey::Character("ab".into()), m),
                None,
                "{m:?} 下不该走文本路径"
            );
        }
        // Shift 不算:Shift+字母本来就是大写字母,是正经文本。
        assert_eq!(
            translate_text(&WKey::Character("AB".into()), ModifiersState::SHIFT).as_deref(),
            Some("AB")
        );
    }

    #[test]
    fn ime_swallows_keys_while_composing_so_pinyin_letters_do_not_leak() {
        // winit 在组字期间**照样**发 `KeyboardInput`(logical_key 就是拼音字母)。
        // 不拦的话打「你好」会先往远端送一串 "nihao",再送「你好」。
        let mut ime = ImeState::default();
        assert!(!ime.swallows_key(), "没在组字时不该吞键");
        ime.on_preedit("ni");
        assert!(ime.swallows_key(), "组字中必须吞掉拼音字母");
        ime.on_commit();
        assert!(
            !ime.swallows_key(),
            "提交后要立刻放行,否则下一个字都打不出来"
        );
    }

    #[test]
    fn empty_preedit_ends_composition() {
        // 用户按 Esc 取消候选:winit 发的是空 preedit,不是 commit。只认 commit
        // 的话组字状态永远挂着,此后**一个键都进不了终端**。
        let mut ime = ImeState::default();
        ime.on_preedit("ni");
        ime.on_preedit("");
        assert!(!ime.swallows_key());
    }

    #[test]
    fn disabling_ime_ends_composition() {
        // 切走输入法 / 失焦时 winit 发 `Ime::Disabled`,同样要解除吞键,
        // 否则用户切回英文输入法后键盘整个失灵。
        let mut ime = ImeState::default();
        ime.on_preedit("ni");
        ime.on_disabled();
        assert!(!ime.swallows_key());
    }

    /// F126:组字中的拼音串必须被留下来 —— 它就是要画到屏幕上的东西。
    #[test]
    fn preedit_text_is_kept_for_rendering() {
        let mut ime = ImeState::default();
        ime.on_preedit("gang'jin");
        assert_eq!(ime.preedit(), "gang'jin");
        assert!(ime.swallows_key(), "组字中照旧吞键");
    }

    /// F126:三条结束边**都**要清空文本。漏一条的现象是屏幕上留一串
    /// 永不消失的幽灵拼音,而且它会一直盖着底下的真实内容。
    ///
    /// 自证会变红:把 `on_commit` / `on_disabled` 里的 `self.text.clear()` 删掉
    /// 任意一句。第三条边(空 `Preedit`)最不直观 —— 它靠的是 `on_preedit` 自己
    /// 那句 `clear()`,删掉它这条测试同样变红。
    #[test]
    fn every_end_of_composition_clears_the_text() {
        for end in ["commit", "empty-preedit", "disabled"] {
            let mut ime = ImeState::default();
            ime.on_preedit("nihao");
            match end {
                "commit" => ime.on_commit(),
                "empty-preedit" => ime.on_preedit(""),
                _ => ime.on_disabled(),
            }
            assert_eq!(ime.preedit(), "", "{end} 之后必须没有残留");
            assert!(!ime.swallows_key(), "{end} 之后不该继续吞键");
        }
    }

    #[test]
    fn ime_commit_normalizes_newlines_to_cr_like_paste_does() {
        // 少数输入法会一次提交带换行的整段。终端要的是 `\r`,送 `\n` 过去
        // shell 不换行(与 `encode_paste` 同一套归一规则)。
        assert_eq!(ime_commit_bytes("a\r\nb\nc"), Some(b"a\rb\rc".to_vec()));
    }

    #[test]
    fn empty_ime_commit_sends_nothing() {
        // 取消候选时某些平台会补一条空 commit;发个空写入是白占一次 channel。
        assert_eq!(ime_commit_bytes(""), None);
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
