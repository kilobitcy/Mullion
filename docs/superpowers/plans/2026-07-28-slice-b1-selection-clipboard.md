# 切片 B1：划选复制 / 粘贴（F18）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让用户能用鼠标在 Mullion 里划选远端终端文本（含跨多屏 scrollback）自动复制到系统剪贴板，并把本地剪贴板内容粘贴进远端（走 bracketed paste，多行且未开 bracketed 时弹窗确认）。

**Architecture:** 选区状态、坐标换算、取文本、粘贴编码全部下沉到 `mullion-term`（纯逻辑，可脱离窗口单测）；`mullion-app` 只做鼠标状态机、剪贴板 IO、弹窗与反色渲染。alacritty 的 `Selection` / `SelectionType` / `Side` / `Point` 不外泄给 app —— app 只传 0-based viewport 单元格坐标和我们自己的 `SelectionKind` / `CellSide`。

**Tech Stack:** Rust workspace；`alacritty_terminal` 0.26（选区）；`arboard` 3.6（剪贴板，已随 egui-winit 进 lock）；`egui` 0.30（粘贴确认弹窗）；`winit` 0.30（鼠标事件）。

**设计 spec:** `docs/superpowers/specs/2026-07-28-slice-b1-selection-clipboard-design.md`（commit `48014d4`）

---

## 文件结构

| 文件 | 责任 | 动作 |
|---|---|---|
| `crates/mullion-term/src/selection.rs` | 选区的对外类型（`SelectionKind` / `CellSide`） | 新建 |
| `crates/mullion-term/src/lib.rs` | 挂 `pub mod selection;` | 改 |
| `crates/mullion-term/src/emulator.rs` | 选区 API（start/update/clear/text）+ snapshot 填 `selected` | 改 |
| `crates/mullion-term/src/snapshot.rs` | `SnapCell` 加 `selected: bool` | 改 |
| `crates/mullion-term/src/keymap.rs` | `encode_paste`（bracketed 包裹 + 净化） | 改 |
| `crates/mullion-app/src/input.rs` | `cell_side` / `click_kind` / `autoscroll_lines` 三个纯函数 | 改 |
| `crates/mullion-app/src/gpu.rs` | `quads_for` 画选中反色底 | 改 |
| `crates/mullion-app/src/text.rs` | `row_to_spans` 选中格用 bg 当文字色 | 改 |
| `crates/mullion-app/src/clipboard.rs` | arboard 封装，失败只 warn | 新建 |
| `crates/mullion-app/src/ui/paste.rs` | 多行粘贴确认弹窗 + 预览纯函数 | 新建 |
| `crates/mullion-app/src/ui/mod.rs` | 挂 `paste` 模块 + `UiState.paste_reply` + `build_ui` 参数 | 改 |
| `crates/mullion-app/src/app.rs` | 鼠标状态机、自动滚动、复制/粘贴触发、弹窗施加点 | 改 |
| `crates/mullion-app/src/lib.rs` | 挂 `pub mod clipboard;` | 改 |
| `crates/mullion-app/Cargo.toml` / 根 `Cargo.toml` | 加 `arboard`（`default-features = false`） | 改 |

## 领域陷阱守护

- **F18 头号坑**：alacritty 的 `Point.line` 是带符号 `Line`，`0` = 当前视口顶行、负数 = 历史。viewport row → buffer line 必须减 `display_offset`（Task 1 的 `scrolled_selection_keeps_pointing_at_same_text` 钉死）。
- **T3/T7**：拖拽自动滚动不许绕开帧闸，靠 `next_frame_at` + `ControlFlow::WaitUntil` 排期（Task 7）。
- **T8**：键盘先判后喂、指针先喂后判的既有规则不动；粘贴弹窗要计入 `modal`。

---

## Task 1: mullion-term 选区 API

**Files:**
- Create: `crates/mullion-term/src/selection.rs`
- Modify: `crates/mullion-term/src/lib.rs`
- Modify: `crates/mullion-term/src/emulator.rs`（在 `impl Emulator` 末尾加方法；测试加在文件底部 `mod tests`）

- [ ] **Step 1: 写选区类型（无测试，纯类型定义）**

新建 `crates/mullion-term/src/selection.rs`：

```rust
//! 选区的对外类型(F18)。
//!
//! alacritty 的 `SelectionType` / `Side` / `Point` **不外泄给 app**:app 只传
//! 0-based viewport 单元格坐标和这里的两个枚举,换算与 alacritty 打交道全在
//! `emulator.rs` 内部。这与 B0 重导出 `TermMode`/`Scroll` 的口径一致——
//! 能封的就封,封不掉的才重导出。

/// 选区类型:拖拽 / 双击选词 / 三击选行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    /// 拖拽:精确按格,不做任何扩展。
    Simple,
    /// 双击:向两侧扩展到最近的语义分隔符(词边界)。
    Semantic,
    /// 三击:整行。
    Lines,
}

/// 指针落在单元格的左半还是右半。决定该格算不算进选区,直接影响"跟手"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSide {
    Left,
    Right,
}
```

在 `crates/mullion-term/src/lib.rs` 的模块列表里加一行（放在 `pub mod palette;` 之后、`pub mod snapshot;` 之前，保持字母序）：

```rust
pub mod selection;
```

- [ ] **Step 2: 写失败的测试**

在 `crates/mullion-term/src/emulator.rs` 底部 `mod tests` 内追加（放在 `alt_screen_has_no_scrollback` 之后）：

```rust
    #[test]
    fn simple_selection_returns_dragged_text() {
        // 从 (0,0) 拖到 (4,0):hello 五个字符全在内。右端取 Right 侧,
        // 否则最后一格落在"格左半"上不算进选区,选出来会少一个字符。
        let mut emu = Emulator::new(20, 3);
        emu.feed(b"hello world");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn empty_selection_is_none() {
        // 没开选区时不能返回 Some("")——上层据此判断"要不要写剪贴板",
        // 返回空串会把用户剪贴板里原有的内容清掉。
        let emu = Emulator::new(20, 3);
        assert_eq!(emu.selection_text(), None);
    }

    #[test]
    fn cleared_selection_is_none() {
        let mut emu = Emulator::new(20, 3);
        emu.feed(b"hello");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert!(emu.selection_text().is_some());
        emu.selection_clear();
        assert_eq!(emu.selection_text(), None);
    }

    #[test]
    fn scrolled_selection_keeps_pointing_at_same_text() {
        // F18 头号坑:alacritty 的 `Line` 带符号,0 = 当前视口顶行、负数是历史。
        // viewport row → buffer line 不减 display_offset 的话,回溯之后在同一个
        // 屏幕位置划选,选出来的是另一段文本(视觉与选区错位)。
        let mut emu = Emulator::new(20, 2);
        emu.feed(b"alpha\r\nbravo\r\ncharlie");
        // 不滚动时:视口是 bravo / charlie,顶行选出 bravo。
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert_eq!(emu.selection_text().as_deref(), Some("bravo"));

        // 回溯一行后视口顶行变成 alpha,同一个屏幕位置必须选出 alpha。
        emu.selection_clear();
        emu.scroll(Scroll::Delta(1));
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert_eq!(
            emu.selection_text().as_deref(),
            Some("alpha"),
            "未减 display_offset:回溯后选区指向了错误的 buffer 行"
        );
    }

    #[test]
    fn semantic_selection_expands_to_word() {
        // 双击选词:点在词中间,向两侧扩到语义分隔符。
        let mut emu = Emulator::new(20, 2);
        emu.feed(b"hello world");
        emu.selection_start(8, 0, SelectionKind::Semantic, CellSide::Left);
        assert_eq!(emu.selection_text().as_deref(), Some("world"));
    }

    #[test]
    fn lines_selection_takes_whole_line() {
        // 三击选行:点在行中任意位置,整行都进选区(alacritty 的 Lines 末尾补 \n)。
        let mut emu = Emulator::new(20, 2);
        emu.feed(b"hello world");
        emu.selection_start(3, 0, SelectionKind::Lines, CellSide::Left);
        assert_eq!(emu.selection_text().as_deref(), Some("hello world\n"));
    }
```

并在 `mod tests` 顶部的 `use super::*;` 之后补 use（`SelectionKind`/`CellSide` 已由 `super::*` 带入，因为下面 Step 3 会在 emulator.rs 顶部 use 它们；无需额外 use）。

- [ ] **Step 3: 跑测试确认失败**

```bash
cargo test -p mullion-term 2>&1 | tail -20
```
预期：编译失败，`no method named 'selection_start' found for struct 'Emulator'`。

- [ ] **Step 4: 实现选区 API**

在 `crates/mullion-term/src/emulator.rs` 顶部 use 区补（放在既有 alacritty use 之后、`use crate::palette;` 之前）：

```rust
use alacritty_terminal::index::{Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};

use crate::selection::{CellSide, SelectionKind};
```

注意既有 `use alacritty_terminal::index::{Column, Line};` 保持不动（`Point`/`Side` 另起一行是为了看清哪些是选区新引入的）。

在 `impl Emulator` 内、`mode()` 之后追加：

```rust
    /// 0-based viewport 单元格 → alacritty 的 buffer `Point`。
    ///
    /// **F18 头号坑**:alacritty 的 `Line` 带符号,`0` = 当前视口顶行、负数是历史。
    /// 回溯之后同一个屏幕位置对应的 buffer 行会变,不减 `display_offset`
    /// 选出来的就是另一段文本(见 `scrolled_selection_keeps_pointing_at_same_text`)。
    ///
    /// 列/行都夹紧在网格内:越界坐标进 `to_range` 会索引到不存在的格。
    fn point_at(&self, col: u16, row: u16) -> Point {
        let grid = self.term.grid();
        let offset = grid.display_offset() as i32;
        let max_col = grid.columns().saturating_sub(1);
        let max_row = grid.screen_lines().saturating_sub(1);
        let row = (row as usize).min(max_row) as i32;
        Point::new(Line(row - offset), Column((col as usize).min(max_col)))
    }

    /// 开始一段选区(左键按下)。`kind` 由 app 侧的连击判定给出。
    pub fn selection_start(&mut self, col: u16, row: u16, kind: SelectionKind, side: CellSide) {
        let point = self.point_at(col, row);
        let ty = match kind {
            SelectionKind::Simple => SelectionType::Simple,
            SelectionKind::Semantic => SelectionType::Semantic,
            SelectionKind::Lines => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(ty, point, side_of(side)));
    }

    /// 更新选区终点(拖拽中)。没有活跃选区时静默忽略。
    pub fn selection_update(&mut self, col: u16, row: u16, side: CellSide) {
        let point = self.point_at(col, row);
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, side_of(side));
        }
    }

    /// 清除选区(点空白、按键、断开连接时调)。
    pub fn selection_clear(&mut self) {
        self.term.selection = None;
    }

    /// 当前选区文本。宽字符、行尾空格裁剪、跨 scrollback 拼接都由上游
    /// `selection_to_string` 负责,不重造。
    ///
    /// 空选区返回 `None` 而非 `Some("")`:上层据此判断要不要写剪贴板,
    /// 空串会把用户剪贴板里原有的内容清掉。
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }
```

在 `impl Emulator` 之后、`#[cfg(test)]` 之前加自由函数：

```rust
/// 我们的 `CellSide` → alacritty 的 `Side`(= `Direction`)。
fn side_of(side: CellSide) -> Side {
    match side {
        CellSide::Left => Side::Left,
        CellSide::Right => Side::Right,
    }
}
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cargo test -p mullion-term 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：`test result: ok.`，全部通过。

若 `semantic_selection_expands_to_word` 或 `lines_selection_takes_whole_line` 的期望字符串与实测不符（上游对行尾/换行的处理），**按实测修正断言并在测试里注明实测结论**，不要改实现去迁就——这两条测的是「上游行为是什么」，不是我们的逻辑。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-term/src/selection.rs crates/mullion-term/src/lib.rs crates/mullion-term/src/emulator.rs
git commit -m "feat(term): 选区 API —— viewport 坐标换算 + 三种选区类型取文本 (F18)

坐标换算减 display_offset 是本片头号坑,由
emulator::tests::scrolled_selection_keeps_pointing_at_same_text 钉死。"
```

---

## Task 2: snapshot 标记选中格

**Files:**
- Modify: `crates/mullion-term/src/snapshot.rs`（`SnapCell` 加字段）
- Modify: `crates/mullion-term/src/emulator.rs`（`snapshot()` 填字段 + 测试）
- Modify: `crates/mullion-app/src/gpu.rs`（测试里的 `SnapCell` 构造补字段）
- Modify: `crates/mullion-app/src/text.rs`（测试里的 `SnapCell` 构造补字段）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-term/src/emulator.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn snapshot_marks_selected_cells() {
        // 反色渲染只能靠这个标记——渲染层拿不到 alacritty 的选区。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"hello");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(2, 0, CellSide::Right);
        let snap = emu.snapshot();
        let marks: Vec<bool> = snap.row(0)[..5].iter().map(|c| c.selected).collect();
        assert_eq!(marks, vec![true, true, true, false, false]);
        // 第二行没选中。
        assert!(snap.row(1).iter().all(|c| !c.selected));
    }

    #[test]
    fn snapshot_marks_wide_char_spacer_as_selected_too() {
        // F16 + F18:选中中文时右半 spacer 也要标中,否则反色底只画半个字。
        let mut emu = Emulator::new(10, 1);
        emu.feed("中x".as_bytes());
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(0, 0, CellSide::Right);
        let snap = emu.snapshot();
        assert!(snap.row(0)[0].selected, "宽字左半应选中");
        assert!(
            snap.row(0)[1].selected,
            "宽字右半 spacer 未标中 → 中文选区只有半个字有底色"
        );
        assert!(!snap.row(0)[2].selected);
    }

    #[test]
    fn snapshot_selection_follows_scroll() {
        // 选区标记与 display_offset 同源:滚动后标记必须跟着内容走,
        // 否则回溯时高亮停在原来的屏幕位置上。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"alpha\r\nbravo\r\ncharlie");
        emu.selection_start(0, 0, SelectionKind::Simple, CellSide::Left);
        emu.selection_update(4, 0, CellSide::Right);
        assert!(emu.snapshot().row(0)[0].selected, "选的是当前顶行 bravo");

        emu.scroll(Scroll::Delta(1));
        let snap = emu.snapshot();
        assert!(
            !snap.row(0)[0].selected,
            "回溯一行后顶行是 alpha,不该还高亮"
        );
        assert!(snap.row(1)[0].selected, "bravo 下移一行,高亮应跟着走");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-term 2>&1 | tail -20
```
预期：编译失败，`no field 'selected' on type '&SnapCell'`。

- [ ] **Step 3: 给 SnapCell 加字段**

`crates/mullion-term/src/snapshot.rs`，在 `spacer` 字段之后追加：

```rust
    /// 是否落在当前选区内(F18)。渲染层据此做反色:背景改用 fg 色、
    /// 文字改用 bg 色。宽字符右半的 spacer 与左半同步标记。
    pub selected: bool,
```

- [ ] **Step 4: 让 snapshot 填充它**

`crates/mullion-term/src/emulator.rs` 的 `snapshot()`：在 `let mut cells = ...` 之后插入选区范围计算，并在 `cells.push(SnapCell { ... })` 里补字段。改后的循环体：

```rust
        let mut cells = Vec::with_capacity(cols * rows);
        // 选区范围与下面的行号换算同源(都基于 display_offset),否则回溯时
        // 高亮会停在原来的屏幕位置上。
        let sel = self
            .term
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&self.term));
        for line in 0..rows {
            let buf_line = Line(line as i32 - offset);
            let row = &grid[buf_line];
            for col in 0..cols {
                let cell = &row[Column(col)];
                let flags = cell.flags;
                let spacer = flags.contains(Flags::WIDE_CHAR_SPACER);
                // 宽字右半自己不在选区范围里(范围按左半的列号),跟随左半标记,
                // 否则中文选中只有半个字有底色。
                let selected = sel.is_some_and(|r| {
                    r.contains(Point::new(buf_line, Column(col)))
                        || (spacer
                            && col > 0
                            && r.contains(Point::new(buf_line, Column(col - 1))))
                });
                cells.push(SnapCell {
                    ch: cell.c,
                    fg: palette::resolve(cell.fg, colors),
                    bg: palette::resolve(cell.bg, colors),
                    width: if flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    },
                    spacer,
                    selected,
                });
            }
        }
```

- [ ] **Step 5: 修 app 侧测试里的 SnapCell 构造**

`crates/mullion-app/src/gpu.rs` 的 `snap_1x1`（约 302 行）与 `crates/mullion-app/src/text.rs` 的 `cell`（约 177 行）都用结构体字面量构造 `SnapCell`，加字段后编译失败。两处都补 `selected: false,`：

`text.rs`：

```rust
    fn cell(ch: char, fg: Rgb, spacer: bool) -> SnapCell {
        SnapCell {
            ch,
            fg,
            bg: Rgb::new(0, 0, 0),
            width: if ch == '中' { 2 } else { 1 },
            spacer,
            selected: false,
        }
    }
```

`gpu.rs` 的 `snap_1x1` 同理，在 `spacer` 之后补 `selected: false,`（若该处未显式写 `spacer`，按实际字段顺序补齐即可）。

- [ ] **Step 6: 跑测试确认通过**

```bash
cargo test --workspace 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：全部 `ok`。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-term/src/snapshot.rs crates/mullion-term/src/emulator.rs crates/mullion-app/src/gpu.rs crates/mullion-app/src/text.rs
git commit -m "feat(term): 快照标记选中格,宽字右半跟随左半 (F18)"
```

---

## Task 3: 粘贴编码 encode_paste

**Files:**
- Modify: `crates/mullion-term/src/keymap.rs`（新增函数 + 测试）

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-term/src/keymap.rs` 底部 `mod tests` 内追加：

```rust
    #[test]
    fn paste_is_bracketed_when_remote_enabled_it() {
        // F18 验收口径(spec.md):开启 bracketed paste 时内容被 ESC[200~ 包裹。
        assert_eq!(
            encode_paste("ls", true),
            b"\x1b[200~ls\x1b[201~".to_vec()
        );
    }

    #[test]
    fn paste_is_raw_when_remote_did_not_enable_bracketed() {
        assert_eq!(encode_paste("ls", false), b"ls".to_vec());
    }

    #[test]
    fn paste_strips_embedded_end_marker_so_it_cannot_break_out() {
        // 真实注入面:粘贴内容里自带 ESC[201~ 就能提前闭合括号,让后半段脱离
        // paste 模式被远端当命令执行。alacritty / wezterm 都防这个。
        let evil = "safe\x1b[201~rm -rf /";
        assert_eq!(
            encode_paste(evil, true),
            b"\x1b[200~saferm -rf /\x1b[201~".to_vec()
        );
        // 未开 bracketed 时同样剔除:标记留在流里对远端也是垃圾字节。
        assert_eq!(encode_paste(evil, false), b"saferm -rf /".to_vec());
    }

    #[test]
    fn paste_normalizes_newlines_to_cr() {
        // 终端里 Enter 是 CR 不是 LF;发 LF 远端 readline 行为会怪
        // (多出空行 / 不执行)。CRLF 也要折成单个 CR,否则每行执行两次。
        assert_eq!(encode_paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-term 2>&1 | tail -20
```
预期：`cannot find function 'encode_paste' in this scope`。

- [ ] **Step 3: 实现**

在 `crates/mullion-term/src/keymap.rs` 里，`encode_key` 之后（`wheel_action` 之前）加：

```rust
/// bracketed paste 的起止标记(DEC 2004)。
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &str = "\x1b[201~";

/// 把一段粘贴文本编码成发往对端的字节(F18)。
///
/// `bracketed` = 远端置了 `TermMode::BRACKETED_PASTE`。开启时用
/// `ESC[200~` / `ESC[201~` 包裹,远端(bash/zsh/Claude Code)据此知道这是
/// 粘贴而非逐键输入,不会把多行内容逐行执行。
///
/// 两件净化,顺序无关但都不能省:
/// 1. **剔除文本里自带的 `ESC[201~`**。不剔的话粘贴内容可以提前闭合括号,
///    让后半段脱离 paste 模式被当命令执行——这是真实注入面,alacritty /
///    wezterm 都防它。
/// 2. **`\r\n` 与 `\n` 统一成 `\r`**。终端里 Enter 是 CR 不是 LF。
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let body = text
        .replace("\r\n", "\r")
        .replace('\n', "\r")
        .replace(PASTE_END, "");
    if !bracketed {
        return body.into_bytes();
    }
    let mut out = Vec::with_capacity(body.len() + PASTE_START.len() + PASTE_END.len());
    out.extend_from_slice(PASTE_START);
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(PASTE_END.as_bytes());
    out
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-term 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：全 `ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/keymap.rs
git commit -m "feat(term): 粘贴编码 —— bracketed 包裹 + 剔除内嵌 ESC[201~ + 换行归一 CR (F18)"
```

---

## Task 4: app 侧三个纯函数

**Files:**
- Modify: `crates/mullion-app/src/input.rs`

- [ ] **Step 1: 写失败的测试**

在 `crates/mullion-app/src/input.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn cell_side_splits_at_half_cell() {
        // 落在格左半 → 该格不算进选区;右半 → 算进去。半格判定直接影响"跟手"。
        assert_eq!(cell_side(0.0, 8.0), CellSide::Left);
        assert_eq!(cell_side(3.9, 8.0), CellSide::Left);
        assert_eq!(cell_side(4.0, 8.0), CellSide::Right);
        assert_eq!(cell_side(7.9, 8.0), CellSide::Right);
        // 下一格重新从左半开始。
        assert_eq!(cell_side(8.0, 8.0), CellSide::Left);
    }

    #[test]
    fn cell_side_survives_zero_cell_width() {
        // cell_w 在字体测量失败时可能是 0;除零会得到 NaN,NaN 比较恒 false,
        // 结果是永远判 Right —— 选区整体偏一格。这里兜底成 Left。
        assert_eq!(cell_side(3.0, 0.0), CellSide::Left);
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
        assert_eq!(autoscroll_lines(-10_000.0, 480.0, 16.0), 5, "必须封顶在 5 行");
    }
```

并在 `mod tests` 顶部的 use 区补：

```rust
    use std::time::Duration;
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app 2>&1 | tail -20
```
预期：`cannot find function 'cell_side' in this scope` 等。

- [ ] **Step 3: 实现**

`crates/mullion-app/src/input.rs`：顶部 use 区补

```rust
use std::time::Instant;

use mullion_term::selection::{CellSide, SelectionKind};
```

在 `cell_at` 之后追加：

```rust
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
pub fn cell_side(px_x: f32, cell_w: f32) -> CellSide {
    // cell_w 为 0(字体测量失败)时除零得 NaN,而 NaN 的比较恒 false,
    // 会一路判成 Right —— 选区整体偏一格。兜底成 1.0。
    let w = if cell_w > 0.0 { cell_w } else { 1.0 };
    let frac = (px_x / w).fract().abs();
    if frac < 0.5 {
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
    (kind, PrevClick { at: now, pos, count })
}

/// 拖拽时指针越出窗口上/下边界要滚几行(F18:选区跨多屏 scrollback)。
///
/// 正数 = 往历史(向上),与 [`mullion_term::emulator::Emulator::scroll`] 的
/// `Scroll::Delta` 语义一致。边界内返回 0。越界越远滚越快,封顶
/// [`AUTOSCROLL_MAX_LINES`]。
pub fn autoscroll_lines(px_y: f32, win_h: f32, cell_h: f32) -> i32 {
    let h = if cell_h > 0.0 { cell_h } else { 1.0 };
    if px_y < 0.0 {
        (((-px_y) / h).ceil() as i32).clamp(1, AUTOSCROLL_MAX_LINES)
    } else if px_y > win_h {
        -((((px_y - win_h) / h).ceil() as i32).clamp(1, AUTOSCROLL_MAX_LINES))
    } else {
        0
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：全 `ok`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/input.rs
git commit -m "feat(app): 划选纯函数 —— 半格判定 / 连击判定 / 边界自动滚动 (F18)"
```

---

## Task 5: 选区反色渲染

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs`（`quads_for` + 测试）
- Modify: `crates/mullion-app/src/text.rs`（`row_to_spans` + 测试）

- [ ] **Step 1: 写失败的测试**

`crates/mullion-app/src/gpu.rs` 的 `mod tests` 内追加（`snap_1x1` 只能造默认 bg 的格，这里自己造带选中标记的快照）：

```rust
    fn snap_selected_1x1(fg: Rgb, bg: Rgb) -> GridSnapshot {
        GridSnapshot {
            cols: 1,
            rows: 1,
            cells: vec![SnapCell {
                ch: 'a',
                fg,
                bg,
                width: 1,
                spacer: false,
                selected: true,
            }],
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: false,
            },
        }
    }

    #[test]
    fn selected_cell_is_inverted_even_on_default_background() {
        // 反色必须优先于「bg 是默认色就不画」这条既有短路,否则在默认背景上
        // (也就是绝大多数情况)选区完全看不见。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let snap = snap_selected_1x1(fg, Rgb::new(0, 0, 0));
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
        assert_eq!(quads.len(), 1, "选中格必须画底色块");
        assert_eq!(quads[0].color, [0xcc, 0xcc, 0xcc], "底色应换成前景色");
    }
```

`crates/mullion-app/src/text.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn selected_cell_draws_text_in_background_color() {
        // 与 gpu::quads_for 的反色底配套:底用 fg、字用 bg,两边必须同时改,
        // 只改一边就是「白底白字」或「黑底黑字」——选中后文字直接消失。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let bg = Rgb::new(0, 0, 0);
        let row = [SnapCell {
            ch: 'a',
            fg,
            bg,
            width: 1,
            spacer: false,
            selected: true,
        }];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].1, to_color(bg));
    }

    #[test]
    fn selection_boundary_splits_spans() {
        // 选中与未选中相邻时颜色不同,必须切成两段,否则整段用同一个颜色画,
        // 高亮边界会错位。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let bg = Rgb::new(0, 0, 0);
        let mk = |ch, selected| SnapCell {
            ch,
            fg,
            bg,
            width: 1,
            spacer: false,
            selected,
        };
        let spans = row_to_spans(&[mk('a', true), mk('b', false)]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, "a");
        assert_eq!(spans[1].0, "b");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p mullion-app 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：`selected_cell_is_inverted_even_on_default_background` 断言失败（`quads.len() == 0`）、`selected_cell_draws_text_in_background_color` 断言失败（拿到的是 fg）。

- [ ] **Step 3: 实现反色底**

`crates/mullion-app/src/gpu.rs` 的 `quads_for`，把内层循环体换成：

```rust
        for (col, cell) in snap.row(row).iter().enumerate() {
            // 宽字右半:底色由左格按 width=2 一并覆盖(选中反色同理),
            // 这里跳过不重复画。
            if cell.spacer {
                continue;
            }
            // F18:选中格画反色底——用前景色当底,文字那趟同步改用 bg 色
            // (见 `text::row_to_spans`)。反色优先于下面「bg 是默认色就不画」
            // 的短路,否则选区在默认背景上完全看不见。
            let color = if cell.selected {
                cell.fg
            } else if cell.bg == default_bg {
                continue;
            } else {
                cell.bg
            };
            quads.push(Quad {
                x: col as f32 * cell_w,
                y: row as f32 * cell_h,
                w: cell.width.max(1) as f32 * cell_w,
                h: cell_h,
                color: [color.r, color.g, color.b],
            });
        }
```

同时把函数文档注释首行改成：

```rust
/// 从快照生成需要画的色块:bg ≠ 默认 的格 + 选中格(反色,F18)+ 可见光标(块状)。
/// 纯函数,可单测。
```

- [ ] **Step 4: 实现文字反色**

`crates/mullion-app/src/text.rs` 的 `row_to_spans`，把 `let color = to_color(cell.fg);` 改成：

```rust
        // F18:选中格反色——底色那趟已用 fg 画底(`gpu::quads_for`),
        // 文字这趟必须同步换成 bg,否则就是同色底同色字,选中后文字消失。
        let color = to_color(if cell.selected { cell.bg } else { cell.fg });
```

- [ ] **Step 5: 跑测试确认通过**

```bash
cargo test --workspace 2>&1 | grep -E "test result|FAILED|panicked"
```
预期：全 `ok`。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/gpu.rs crates/mullion-app/src/text.rs
git commit -m "feat(app): 选区反色渲染 —— 底用 fg / 字用 bg,两趟同步改 (F18)"
```

---

## Task 6: 剪贴板封装

**Files:**
- Modify: `Cargo.toml`（workspace 依赖）
- Modify: `crates/mullion-app/Cargo.toml`
- Create: `crates/mullion-app/src/clipboard.rs`
- Modify: `crates/mullion-app/src/lib.rs`

本 Task 全是平台 IO，没有可单测的逻辑；验收标准是「编译过 + clippy 干净 + 交叉编译到 Windows 仍干净」。

- [ ] **Step 1: 加依赖**

根 `Cargo.toml` 的 `[workspace.dependencies]` 末尾（`log = "0.4"` 之后）加：

```toml
# 切片 B1(F18):系统剪贴板。已作为 egui-winit 的传递依赖在 Cargo.lock 里
# (3.6.1),加直接依赖不引入新版本。关掉默认 features:image-data 会拉进
# image crate(N6 的 exe 体积已超标),我们只要文本。
arboard = { version = "3.6", default-features = false }
```

`crates/mullion-app/Cargo.toml` 的 `[dependencies]` 里，`log.workspace = true` 之后加：

```toml
arboard.workspace = true
```

- [ ] **Step 2: 确认依赖能解析且没引入新版本**

```bash
cargo tree -p mullion-app -i arboard 2>&1 | head -20
git diff --stat Cargo.lock
```
预期：`arboard v3.6.1` 只有一个版本；`Cargo.lock` 若有改动只应是依赖关系边，不应出现新增的 `image` / `png` 之类条目。若 lock 里冒出 image 相关 crate，说明 `default-features = false` 没生效，停下检查。

- [ ] **Step 3: 写剪贴板封装**

新建 `crates/mullion-app/src/clipboard.rs`：

```rust
//! 系统剪贴板(F18)。薄封装,把「失败」这件事一次性处理掉。
//!
//! 不用 egui 的剪贴板:egui 只有 `copy_text`,**读剪贴板只能靠 `Event::Paste`
//! 且要 egui 持有焦点**。按 T8 的教训(egui 焦点系统吞掉 Tab,终端永久收不到键),
//! 不让 egui 掺和终端输入路径。
//!
//! 所有失败一律 `log::warn!` + 忽略:Windows 上剪贴板被别的进程短暂占用是常态,
//! 复制失败最多是用户再选一次,不值得弹窗打断,更不值得 panic。

/// 系统剪贴板句柄。打开失败时内部是 `None`,读写退化成 no-op。
pub struct Clipboard {
    inner: Option<arboard::Clipboard>,
}

impl Clipboard {
    /// 打开剪贴板。失败只记一行日志——GUI 已经起来了,不该因为剪贴板起不来而崩。
    pub fn new() -> Self {
        let inner = match arboard::Clipboard::new() {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!(target: "mullion", "剪贴板不可用,复制/粘贴将被忽略: {e}");
                None
            }
        };
        Self { inner }
    }

    /// 写入文本。空串不写:那等于把用户剪贴板里原有的内容清掉。
    pub fn set(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let Some(c) = self.inner.as_mut() else { return };
        if let Err(e) = c.set_text(text.to_owned()) {
            log::warn!(target: "mullion", "写剪贴板失败: {e}");
        }
    }

    /// 读出文本。剪贴板为空、内容不是文本(图片/文件)、或被占用都返回 `None`。
    pub fn get(&mut self) -> Option<String> {
        let c = self.inner.as_mut()?;
        match c.get_text() {
            Ok(t) => Some(t),
            Err(e) => {
                log::warn!(target: "mullion", "读剪贴板失败: {e}");
                None
            }
        }
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}
```

`crates/mullion-app/src/lib.rs` 的模块列表里加（保持字母序，放在 `pub mod cli;` 之后）：

```rust
pub mod clipboard;
```

- [ ] **Step 4: 编译 + clippy**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -20
```
预期：无输出（无警告）。

- [ ] **Step 5: 交叉编译验收（arboard 会不会破坏 Windows 目标）**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -20
```
预期：编译成功。若失败，看错误是否来自 arboard 的 Windows 后端（`clipboard-win`），按 `docs/cross-compile-windows.md` 处理；连续两次改不过就停下问用户，别猜。

顺手量一次体积（设计 spec 的风险项：N6 目标 25MB，v0.1.7 已是 33MB）：

```bash
ls -l target/x86_64-pc-windows-gnu/release/mullion.exe
```
把数字记在本步骤旁边。**本片不做体积优化**，只记录增量。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml Cargo.lock crates/mullion-app/Cargo.toml crates/mullion-app/src/clipboard.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 系统剪贴板封装,失败只 warn 不打断 (F18)"
```

---

## Task 7: 鼠标状态机与复制/粘贴接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

事件循环本身没有可测逻辑（决策全在 Task 1/4 的纯函数里），本 Task 靠 `cargo test --workspace` 不回归 + 人工验收。

- [ ] **Step 1: 加字段**

`crates/mullion-app/src/app.rs` 的 `pub struct App`，在 `cursor_px` 之后追加：

```rust
    /// 系统剪贴板(F18)。打不开时内部退化为 no-op(见 `crate::clipboard`)。
    clipboard: crate::clipboard::Clipboard,
    /// 左键是否按住(划选进行中)。松开即结束,不跨 focus 保留。
    dragging: bool,
    /// 上一次左键按下的连击状态,喂 `input::click_kind` 判双击/三击。
    prev_click: Option<input::PrevClick>,
    /// 拖拽出界时每帧要滚的行数;0 = 不自动滚。**只在真正 present 的那一帧施加**
    /// (见 `RedrawRequested` 里的说明),否则重演 T3/T7。
    autoscroll: i32,
    /// 待用户确认的多行粘贴(F18)。`Some` = 弹窗开着,计入 `modal`(T8)。
    pending_paste: Option<String>,
```

`App::new` 的初始化列表里，`cursor_px: (0.0, 0.0),` 之后追加：

```rust
            clipboard: crate::clipboard::Clipboard::new(),
            dragging: false,
            prev_click: None,
            autoscroll: 0,
            pending_paste: None,
```

- [ ] **Step 2: 加选区/剪贴板方法**

在 `impl App` 内、`fn request_ui_redraw` 之后插入：

```rust
    /// 指针当前位置对应的 **0-based** viewport 单元格与格内左右半。
    ///
    /// `input::cell_at` 给的是 **1-based**(F17 鼠标上报的口径,SGR 协议要求),
    /// 而选区 API 收 0-based。两套口径并存是既有事实,换算**只在这一个函数里做**,
    /// 别让 0/1 混进事件循环——那是 off-by-one 最容易长出来的地方。
    fn selection_cursor(&self) -> Option<(u16, u16, mullion_term::selection::CellSide)> {
        let a = self.active.as_ref()?;
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let (col1, row1) = input::cell_at(self.cursor_px, cell_px, a.grid_dims);
        let side = input::cell_side(self.cursor_px.0, cell_px.0);
        Some((col1.saturating_sub(1), row1.saturating_sub(1), side))
    }

    /// 左键按下:判连击类型 → 开新选区(旧选区被覆盖)。
    fn selection_press(&mut self) {
        let Some(a) = self.active.as_ref() else { return };
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let pos1 = input::cell_at(self.cursor_px, cell_px, a.grid_dims);
        let (kind, prev) = input::click_kind(self.prev_click, Instant::now(), pos1);
        self.prev_click = Some(prev);
        if let Some((col, row, side)) = self.selection_cursor() {
            if let Some(conn) = self.conn.as_mut() {
                conn.pane.emulator.selection_start(col, row, kind, side);
            }
        }
        self.dragging = true;
        self.request_ui_redraw();
    }

    /// 更新选区终点 + 重算出界滚动量。**不请求重绘**:自动滚动那条路径要在
    /// present 之后调它,在那里 `request_redraw` 会与 `RedrawRequested` 互相触发,
    /// 绕开帧闸忙转(T3/T7)。需要重绘的调用方自己调 `request_ui_redraw`。
    fn update_selection_endpoint(&mut self) {
        let Some(a) = self.active.as_ref() else { return };
        let win_h = a.gpu.config.height as f32;
        let cell_h = a.text.cell_h;
        self.autoscroll = input::autoscroll_lines(self.cursor_px.1, win_h, cell_h);
        if let Some((col, row, side)) = self.selection_cursor() {
            if let Some(conn) = self.conn.as_mut() {
                conn.pane.emulator.selection_update(col, row, side);
            }
        }
    }

    /// 左键松开:选中即复制(PuTTY / Xshell 习惯,F18 交互口径)。
    fn selection_release(&mut self) {
        self.dragging = false;
        self.autoscroll = 0;
        self.copy_selection();
    }

    /// 把当前选区写进系统剪贴板。无选区 = 什么都不做(`selection_text` 返回
    /// `None`),不能写空串——那会清掉用户剪贴板里原有的内容。
    fn copy_selection(&mut self) {
        let Some(text) = self
            .conn
            .as_ref()
            .and_then(|c| c.pane.emulator.selection_text())
        else {
            return;
        };
        self.clipboard.set(&text);
    }

    /// 右键 / `Ctrl+Shift+V`:读剪贴板 → 判断要不要先确认 → 发送。
    fn request_paste(&mut self) {
        let Some(text) = self.clipboard.get() else { return };
        if text.is_empty() {
            return;
        }
        let bracketed = self.conn.as_ref().is_some_and(|c| {
            c.pane
                .emulator
                .mode()
                .contains(mullion_term::TermMode::BRACKETED_PASTE)
        });
        // 只在「含换行 **且** 远端没开 bracketed paste」时确认:这种组合下每个换行
        // 都会被远端当回车执行,是唯一真正危险的情形。在 Claude Code 里贴代码
        // (bracketed 已开)必须无感——每次都弹的话,这功能比没有还烦。
        if !bracketed && text.contains('\n') {
            self.pending_paste = Some(text);
            self.request_ui_redraw();
            return;
        }
        self.send_paste(&text);
    }

    /// 真正发送。到这里要么不需要确认,要么用户已经点了「粘贴」。
    fn send_paste(&mut self, text: &str) {
        let Some(conn) = self.conn.as_mut() else { return };
        let bracketed = conn
            .pane
            .emulator
            .mode()
            .contains(mullion_term::TermMode::BRACKETED_PASTE);
        let bytes = mullion_term::keymap::encode_paste(text, bracketed);
        // 与按键同理(F17):贴之前先回底部,否则「贴了但看不到」。
        conn.pane.emulator.scroll_to_bottom();
        let _ = conn.ssh.write(bytes);
    }
```

- [ ] **Step 3: 接鼠标事件**

`window_event` 的 `modal` 计算里加上粘贴弹窗（T8：弹窗开着时键盘归 egui）：

```rust
            let modal = self.ui.session_manager_open
                || self.ui.about_open
                || self.ui.editor_open
                || self.pending_host_key.is_some()
                || self.pending_paste.is_some();
```

`WindowEvent::CursorMoved` 分支改成：

```rust
            // 指针坐标只在这里更新;滚轮上报要用(F17),划选要用(F18)。
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_px = (position.x as f32, position.y as f32);
                if self.dragging {
                    self.update_selection_endpoint();
                    self.request_ui_redraw();
                }
            }
```

在 `WindowEvent::MouseWheel` 分支之后、`WindowEvent::Resized` 分支之前，插入新分支：

```rust
            // F18 划选 / 右键粘贴。
            //
            // 鼠标**按键**上报(F15)本片不做,所以左键无条件走本地划选,不需要
            // T5 的 Shift 逃生门分流;将来加按键上报时,分流点就在这里
            // (与上面 MouseWheel 的 `wheel_action` 同构)。
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.selection_press(),
                (MouseButton::Left, ElementState::Released) => self.selection_release(),
                // 右键直接贴,不弹菜单(Windows 终端习惯,F18 交互口径)。
                (MouseButton::Right, ElementState::Pressed) => self.request_paste(),
                _ => {}
            },
```

在 app.rs 顶部的 winit import 里补 `MouseButton`（`ElementState` 已在用）：找到形如
`use winit::event::{ElementState, WindowEvent};` 的那行，改成
`use winit::event::{ElementState, MouseButton, WindowEvent};`（以文件实际写法为准，只加 `MouseButton` 一项）。

- [ ] **Step 4: 接键盘（Ctrl+Shift+C/V + 按键清选区）**

`WindowEvent::KeyboardInput` 分支里，在 `if let Some((key, mods)) = input::translate_key(...)` 之后、Shift+PageUp 那段**之前**插入：

```rust
                        // F18:`Ctrl+Shift+C/V` 必须在 `encode_key` 之前截住。
                        // Ctrl+C 会被编码成 `0x03`(SIGINT)——漏下去就是「想复制
                        // 结果把远端进程杀了」。Shift 让它与裸 Ctrl+C 明确区分,
                        // 裸 Ctrl+C 照旧转发。
                        if mods.ctrl && mods.shift {
                            if let Key::Char(c) = key {
                                match c.to_ascii_lowercase() {
                                    'c' => {
                                        self.copy_selection();
                                        self.request_ui_redraw();
                                        return;
                                    }
                                    'v' => {
                                        self.request_paste();
                                        self.request_ui_redraw();
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                        }
```

同一分支里，发送按键那段加一行清选区：

```rust
                        if let Some(conn) = self.conn.as_mut() {
                            // F18:一按普通键就清选区。留着的话高亮会挂在屏幕上,
                            // 而底下的内容早被新输出冲掉了——高亮的是别的字。
                            conn.pane.emulator.selection_clear();
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            conn.pane.emulator.scroll_to_bottom();
                            let _ = conn.ssh.write(bytes);
                        }
```

- [ ] **Step 5: 接自动滚动（严格挂在帧上）**

`WindowEvent::RedrawRequested` 分支里，把

```rust
                match self.limiter.plan(dirty, now) {
```

改成

```rust
                let action = self.limiter.plan(dirty, now);
                // F18 自动滚动只在**真正出帧**的那一轮施加,见 match 之后的说明。
                let presented = matches!(action, RedrawAction::Present);
                match action {
```

（三个分支的内容一字不动。）

然后在 `match` 整块结束之后、`// Task 6:会话管理弹窗的 intent 施加点` 那段**之前**，插入：

```rust
                // F18:拖拽出界时的自动滚动,让选区能跨越多屏 scrollback。
                //
                // 位置很讲究,三个都不能选:
                // - 挂在 `CursorMoved` 上 → 频率是鼠标事件频率,一甩就滚飞;
                // - 挂在 match 之后但不判 `presented` → Throttle 轮也会滚,而下面的
                //   排期又会唤醒下一轮,变成「一轮滚一次」的忙转(T3/T7 红线);
                // - 在这里调 `request_ui_redraw` → 它内含 `request_redraw`,同样会与
                //   `RedrawRequested` 互相触发绕开帧闸。
                //
                // 所以:只在 present 过的那一轮滚一次(频率 = 帧率 ~60fps),只标脏 +
                // 经 next_frame_at/WaitUntil 排期,由 `about_to_wait` 到点补画。
                if presented && self.dragging && self.autoscroll != 0 {
                    let lines = self.autoscroll;
                    if let Some(conn) = self.conn.as_mut() {
                        conn.pane.emulator.scroll(Scroll::Delta(lines));
                    }
                    // 滚动改了 display_offset,选区终点要按新视口重新落点,
                    // 否则拖到边缘后画面在滚、选区却停在原地不长。
                    self.update_selection_endpoint();
                    self.ui_dirty = true;
                    let at = Instant::now() + std::time::Duration::from_millis(16);
                    self.next_frame_at = Some(at);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(at));
                }
```

- [ ] **Step 6: 跑全量测试 + clippy**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```
预期：测试全 `ok`；clippy 无输出。

**重点确认没回归的守护测试**（改了事件循环，按 CLAUDE.md 要求点名）：
`app::tests::redraw_is_frame_capped`（T3）、`frame::tests` 的 `plan` 四条（T7）、
`shell::input_route::tests::terminal_keyboard_is_never_fed_to_egui_so_tab_cannot_steal_focus`（T8）。

```bash
cargo test -p mullion-app frame:: input_route:: redraw_is_frame_capped 2>&1 | grep -E "test result|FAILED"
```

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 鼠标划选状态机 + 选中即复制 + 右键/Ctrl+Shift+V 粘贴 (F18)

自动滚动只在 present 过的那一轮施加、只经 next_frame_at/WaitUntil 排期,
不 request_redraw —— 守 T3/T7。跑了 frame::tests、redraw_is_frame_capped、
input_route 的 T8 守护测试。"
```

---

## Task 8: 多行粘贴确认弹窗

**Files:**
- Create: `crates/mullion-app/src/ui/paste.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs`
- Modify: `crates/mullion-app/src/app.rs`

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-app/src/ui/paste.rs`，先只放测试（实现下一步写）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_shows_all_lines_when_short() {
        let (text, total) = preview_of("a\nb\nc");
        assert_eq!(total, 3);
        assert_eq!(text, "a\nb\nc\n");
        assert!(!text.contains("还有"), "行数没超上限不该出现省略提示");
    }

    #[test]
    fn preview_truncates_long_input_and_reports_remainder() {
        // 用户贴进来的可能是几千行的日志,预览必须有上限,否则弹窗把屏幕撑满、
        // 「取消」按钮被挤到窗外——反而点不掉。
        let input = (1..=20).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let (text, total) = preview_of(&input);
        assert_eq!(total, 20);
        assert!(text.starts_with("1\n2\n3\n4\n5\n"));
        assert!(text.contains("还有 15 行"));
        assert!(!text.contains("\n6\n"), "超出上限的行不该出现在预览里");
    }

    #[test]
    fn preview_clips_overlong_single_line() {
        // 一行几万字符(minified js / base64)同样能把窗撑爆。
        let long = "x".repeat(500);
        let (text, total) = preview_of(&long);
        assert_eq!(total, 1);
        assert!(text.chars().count() < 200, "超长行必须截断");
        assert!(text.contains('…'), "截断处要有省略号,别让用户以为就这么多");
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

模块还没挂上，先在 `crates/mullion-app/src/ui/mod.rs` 顶部模块列表加（字母序，`host_key` 之后）：

```rust
pub mod paste;
```

```bash
cargo test -p mullion-app paste 2>&1 | tail -20
```
预期：`cannot find function 'preview_of' in this scope`。

- [ ] **Step 3: 实现弹窗**

把 `crates/mullion-app/src/ui/paste.rs` 的测试模块之前补上实现（整个文件如下，测试模块保持 Step 1 的内容放在末尾）：

```rust
//! 多行粘贴确认弹窗(F18)。
//!
//! 只在「粘贴内容含换行 **且** 远端没开 bracketed paste」时出现:这种组合下每个
//! 换行都会被远端当回车执行,一次误贴能连着跑好几条命令。其余情况直接粘贴——
//! 在 Claude Code 里贴代码(bracketed 已开)必须无感。
//!
//! 与主机密钥弹窗(`host_key.rs`,故意不给关闭按钮)相反,这个窗**可以取消**:
//! 取消 = 不粘贴,是明确且安全的默认,没有「以为没事发生、其实还挂着」的歧义。

/// 预览最多显示几行。
const PREVIEW_LINES: usize = 5;
/// 每行预览最多显示几个字符。一行几万字符(minified js / base64)同样能撑爆窗。
const PREVIEW_COLS: usize = 120;

/// 生成预览文本与总行数。纯函数,可单测。
pub fn preview_of(text: &str) -> (String, usize) {
    let total = text.lines().count();
    let mut out = String::new();
    for line in text.lines().take(PREVIEW_LINES) {
        // 按字符数而非字节数截断:按字节切会把多字节字符切成两半(panic 或乱码)。
        if line.chars().count() > PREVIEW_COLS {
            out.extend(line.chars().take(PREVIEW_COLS));
            out.push('…');
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if total > PREVIEW_LINES {
        out.push_str(&format!("… 还有 {} 行", total - PREVIEW_LINES));
    }
    (out, total)
}

/// 弹窗要展示的只读视图。借用式,与 `host_key::HostKeyView` 同构。
#[derive(Clone, Copy)]
pub struct PasteView<'a> {
    pub text: &'a str,
}

/// 画弹窗。用户做出选择时把 `Some(accept)` 写进 `reply`,由 app.rs 事后施加
/// (取出 `pending_paste` 并发送)——egui 闭包里借不到 `&mut App`。
///
/// 用 `egui::Modal` 而非 `egui::Window`:普通 `Window` 不挡下层点击,弹窗开着时
/// 用户不该还能往终端里打字(与 F3 主机密钥弹窗同一理由)。
pub fn show(ctx: &egui::Context, view: &PasteView<'_>, reply: &mut Option<bool>) {
    let (preview, total) = preview_of(view.text);
    egui::Modal::new(egui::Id::new("paste_confirm")).show(ctx, |ui| {
        // Modal 没有标题栏(不像 Window),标题得自己画。
        ui.heading("确认粘贴多行内容");
        ui.separator();
        ui.label(format!(
            "剪贴板里有 {total} 行。远端没有开启 bracketed paste,\
             每个换行都会被当成回车直接执行。"
        ));
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(160.0)
            .show(ui, |ui| {
                ui.monospace(preview);
            });
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("粘贴").clicked() {
                *reply = Some(true);
            }
            if ui.button("取消").clicked() {
                *reply = Some(false);
            }
        });
        // Esc 等价于取消。Modal 没有标题栏的 X,不给键盘出口的话用户只能靠鼠标。
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *reply = Some(false);
        }
    });
}
```

- [ ] **Step 4: 跑测试确认通过**

```bash
cargo test -p mullion-app paste 2>&1 | grep -E "test result|FAILED"
```
预期：`test result: ok. 3 passed`。

- [ ] **Step 5: 接进 UiState / build_ui**

`crates/mullion-app/src/ui/mod.rs` 的 `UiState`，在 `host_key_reply` 之后追加：

```rust
    /// 多行粘贴确认弹窗的回答(F18)。`Some(true)` = 粘贴;`Some(false)` = 取消。
    /// 同样只承载意图:取出 `pending_paste` 并发送在 app.rs 施加点做。
    pub paste_reply: Option<bool>,
```

`build_ui` 的签名加一个参数（放在 `host_key` 之后）：

```rust
pub fn build_ui(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    store_available: bool,
    connected: bool,
    status: &str,
    host_key: Option<host_key::HostKeyView<'_>>,
    paste: Option<paste::PasteView<'_>>,
) {
```

函数体里，主机密钥那段之后加：

```rust
    // 粘贴确认排在主机密钥之后:安全关口优先级最高,粘贴其次。
    // 两者同时出现的可能性极低(握手期间还没有终端可粘),但顺序要写死,
    // 别留给 egui 的绘制顺序去决定谁盖谁。
    if let Some(view) = &paste {
        paste::show(ctx, view, &mut ui_state.paste_reply);
    }
```

- [ ] **Step 6: 接进 app.rs**

`render_frame` 的签名加参数（已有 `#[allow(clippy::too_many_arguments)]`，不必改属性）：

```rust
fn render_frame(
    a: &mut Active,
    pane: Option<&Pane>,
    ui_state: &mut crate::ui::UiState,
    sessions: &[mullion_store::SessionRecord],
    store_available: bool,
    connected: bool,
    status: &str,
    host_key: Option<crate::ui::host_key::HostKeyView<'_>>,
    paste: Option<crate::ui::paste::PasteView<'_>>,
) -> std::time::Duration {
```

函数体里的 `crate::ui::build_ui(...)` 调用补最后一个实参 `paste,`。

`RedrawRequested` 的 `Present` 分支里，`host_key_view` 之后加视图构造，并把它传进 `render_frame`：

```rust
                            // 与 host_key_view 同理:`self.pending_paste` 与
                            // `&mut self.ui` 是不相干字段,可同时借出。
                            let paste_view = self
                                .pending_paste
                                .as_deref()
                                .map(|text| crate::ui::paste::PasteView { text });
                            let repaint_delay = render_frame(
                                a,
                                pane,
                                &mut self.ui,
                                sessions,
                                store_available,
                                connected,
                                &status,
                                host_key_view,
                                paste_view,
                            );
```

在 intent 施加点（`host_key_reply` 那段之后、`match` 大括号闭合之前）加：

```rust
                // F18:粘贴确认弹窗的回答。放在这里而不是 egui 闭包里——发送要
                // `&mut self.conn`,闭包里借不到(与会话管理器/主机密钥同构)。
                if let Some(accept) = self.ui.paste_reply.take() {
                    if let Some(text) = self.pending_paste.take() {
                        if accept {
                            self.send_paste(&text);
                        }
                    }
                }
```

- [ ] **Step 7: 全量绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --check
```
预期：测试全 `ok`；clippy 无输出；fmt 无输出。

- [ ] **Step 8: 提交**

```bash
git add crates/mullion-app/src/ui/paste.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 多行粘贴确认弹窗(仅未开 bracketed paste 时弹) (F18)"
```

---

## Task 9: 交付 v0.1.8

按 `CLAUDE.md` 的「交付约定」一条龙做完，不中途问。

**Files:**
- Modify: `Cargo.toml`（`workspace.package.version`）
- Modify: `spec.md`（F18 标记完成）

- [ ] **Step 1: 更新 spec.md**

`spec.md` 第 94 行的 F18 那行，按仓库既有的完成标记写法（参照 F17 / F3 在切片 B0 之后的写法）标为已实现，并把验收口径补成实际的测试名：

```markdown
| F18 | 划选复制 / 粘贴，粘贴走 bracketed paste | P1 | ✅ v0.1.8。单测：`keymap::tests::paste_is_bracketed_when_remote_enabled_it`、`emulator::tests::scrolled_selection_keeps_pointing_at_same_text` |
```

若仓库里 F17/F3 用的是别的标记法（如单独一节列「已实现」），**跟随既有写法**，不要新造一种。先看一眼：

```bash
grep -n "F17\|F3 " spec.md | head -20
```

- [ ] **Step 2: 升版本 + 单独提交**

`Cargo.toml` 的 `[workspace.package]`：`version = "0.1.7"` → `version = "0.1.8"`。

```bash
cargo check --workspace 2>&1 | tail -3   # 让 Cargo.lock 里的版本号同步更新
git add Cargo.toml Cargo.lock spec.md
git commit -m "chore: 版本 0.1.8(划选复制 + 粘贴 F18)"
```

- [ ] **Step 3: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```
三条全干净才叫绿。**不绿不发。**

- [ ] **Step 4: 交叉编译 + objdump 依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
```

按 `docs/cross-compile-windows.md` 做依赖验收：

```bash
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion.exe \
  | grep -i "DLL Name"
```
出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` 即为**不合格**，必须按运行手册修（静态链接 runtime），不能拿去发。

- [ ] **Step 5: 发 Release**

```bash
cd target/x86_64-pc-windows-gnu/release
sha256sum mullion.exe > mullion.exe.sha256
```

写 `notes.md`（内容见下），然后：

```bash
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.8 \
  target/x86_64-pc-windows-gnu/release/mullion.exe \
  target/x86_64-pc-windows-gnu/release/mullion.exe.sha256 \
  -t "v0.1.8" -F notes.md --repo kilobitcy/Mullion
```

**标题只能是纯版本号 `v0.1.8`**，不带破折号、摘要、emoji。

`notes.md` 必须包含的**人工验收清单**（这些在无头容器里验不了，见 CLAUDE.md「你无法验证的东西」）：

```markdown
## 本版新增：划选复制 / 粘贴（F18）

- 左键拖拽划选，反色高亮
- 双击选词 / 三击选行
- 拖到窗口上/下边缘自动滚动，选区可跨多屏 scrollback
- 松开左键即复制到系统剪贴板（无需按键）；`Ctrl+Shift+C` 同效
- 右键粘贴；`Ctrl+Shift+V` 同效；远端开启 bracketed paste 时用 `ESC[200~` 包裹
- 粘贴内容含换行且远端未开 bracketed paste 时，弹窗确认（可取消）

## 人工验收清单

- [ ] 反色高亮在默认背景与已有背景色的单元格上都清晰可读
- [ ] 拖拽跟手；松开瞬间选区与视觉一致（不多不少半格）
- [ ] 拖到上/下边缘自动滚动的速度顺手，不冲过头
- [ ] 选区跨多屏 scrollback 后，复制出来的文本连续、无缺行
- [ ] 双击选词的边界符合直觉（路径 / URL / 带下划线的标识符）
- [ ] CJK 宽字符整字选中，不出现半个字
- [ ] 复制到记事本 / 浏览器正常；从浏览器复制内容粘贴进来正常
- [ ] 在 Claude Code 里贴多行代码：不弹确认窗（bracketed 已开），内容不被逐行执行
- [ ] 在裸 shell 里贴多行文本：弹确认窗，「取消」真的不粘贴、Esc 也能取消
- [ ] 右键粘贴不与 Windows 输入法的右键行为打架
- [ ] 划选 / 自动滚动期间 CPU 占用正常（不出现风扇起飞 —— T3/T7 回归信号）

## sha256

<粘贴 mullion.exe.sha256 的内容>
```

- [ ] **Step 6: 报给用户**

Release 链接 + sha256 + 上面的验收清单。

---

## 收尾（用户实机验收通过之后再做）

按 `github-integration-ops` 的既定流程：

1. 建本地备份分支留档（**永不推送**）：`git branch backup/pre-squash-b1`
2. `git checkout main && git merge --squash feat/slice-b1-selection-clipboard`
3. **推之前必扫暂存内容**，确认没有真机 IP / 用户名 / 私钥路径 / 凭据
4. `GIT_SSH_COMMAND='ssh -o "ProxyCommand=nc -X 5 -x 127.0.0.1:7891 %h %p" -o StrictHostKeyChecking=accept-new' git push origin main`

只推脱敏后的 main，不推 feature 分支。
