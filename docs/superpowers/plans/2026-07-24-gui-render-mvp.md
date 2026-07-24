# GUI 渲染 MVP 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 单 pane 铺满窗口、系统字体、基础前景/背景色 + 块状光标、键盘输入、resize→window_change,连真实 SSH 显示并操作远端 tmux(spec.md v0.1 里程碑)。

**Architecture:** term 加纯数据 `snapshot()`(xterm-256 调色板解析 + CJK 宽字标记)与 `resize()`;app 拥有 tokio 运行时(ADR-004 方案 B),winit `ApplicationHandler<UserEvent>` 事件循环里每帧「排空 rx → feed emu → 回写 PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸;渲染两趟(wgpu 色块背景 + glyphon 文字前景)。

**Tech Stack:** winit 0.30.13 / wgpu 23.0.1 / glyphon 0.7.0 / cosmic-text 0.12.1 / alacritty_terminal 0.26 / tokio 1(多线程)。

**关键前提(已核锁定源码):** `Term::colors()` 默认全 `None`,默认 RGB 归我们所有;`grid[Line(i)][Column(j)] -> &Cell`;`Cell{c,fg,bg,flags}`,`Flags::{WIDE_CHAR,WIDE_CHAR_SPACER,LEADING_WIDE_CHAR_SPACER}`;`grid.cursor.point: Point<Line(i32),Column(usize)>`;`alacritty_terminal::vte::ansi::{Color::{Named(NamedColor),Spec(Rgb),Indexed(u8)}, Rgb{r,g,b:u8}, NamedColor(Foreground=256,Background=257)}`;`Term::resize<S: Dimensions>(size)`;`glyphon::Color::rgb(r,g,b)`;`Buffer::set_rich_text(spans, default_attrs, shaping)`;winit `KeyEvent{logical_key: Key, state}`、`Key::{Named(NamedKey::Enter),Character(SmolStr)}`、`ModifiersState::{shift_key,control_key,alt_key,super_key}`。

**「绿」的定义:** `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings` 无输出 **且** `cargo fmt --check` 干净。每个任务最后一步都要满足。

**GPU/人眼类免责:** Task 8 的 wgpu/glyphon 胶水与端到端窗口行为**无法在无头容器自动验证**。这些步骤守护 = 编译通过 + `cargo run` 能起窗口;正确性(字形位置、颜色、是否不闪、CJK 对齐)进 PR 的人工验证清单,**不编造通过结论**。

---

## 文件结构

**term(新增纯数据,不违反「term 不依赖 core/ssh/app」)**
- 新建 `crates/mullion-term/src/snapshot.rs` — `Rgb / SnapCell / Cursor / GridSnapshot` 纯数据。
- 新建 `crates/mullion-term/src/palette.rs` — xterm-256 默认调色板 + `resolve(Color,&Colors)->Rgb`。
- 改 `crates/mullion-term/src/emulator.rs` — 加 `snapshot()` 与 `resize()`。
- 改 `crates/mullion-term/src/lib.rs` — 注册两个新模块。

**app**
- 新建 `crates/mullion-app/src/grid.rs` — `grid_size_for` 纯函数。
- 新建 `crates/mullion-app/src/cli.rs` — 解析 `user@host -p N -i key` → `SshConfig`。
- 新建 `crates/mullion-app/src/text.rs` — `row_to_spans`(纯)+ glyphon 文字层(胶水)。
- 新建 `crates/mullion-app/src/gpu.rs` — `quads_for`(纯)+ wgpu 表面/色块管线(胶水)。
- 新建 `crates/mullion-app/src/input.rs` — `translate_key`(winit→keymap,纯)。
- 改 `crates/mullion-app/src/app.rs` — `App`(`ApplicationHandler<UserEvent>`)实体。
- 改 `crates/mullion-app/src/main.rs` — EventLoop + 连接接线。
- 改 `crates/mullion-app/src/lib.rs` — 注册新模块。

---

## Task 1: term — 渲染快照数据类型 + xterm-256 调色板解析

**Files:**
- Create: `crates/mullion-term/src/snapshot.rs`
- Create: `crates/mullion-term/src/palette.rs`
- Modify: `crates/mullion-term/src/lib.rs`

- [ ] **Step 1: 建快照纯数据类型(先建类型,供后续任务引用)**

写 `crates/mullion-term/src/snapshot.rs`:

```rust
//! 渲染快照:纯数据网格,供 app 渲染。零 UI 依赖(架构不变量:term 不依赖 app)。

/// 8-bit RGB。渲染层再转 glyphon/wgpu 颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// 单元格快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapCell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    /// 显示宽度:CJK 宽字符 = 2,其余 = 1(F16)。
    pub width: u8,
    /// 宽字符右半的占位格:渲染时跳过,不重复画。
    pub spacer: bool,
}

/// 光标快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// 一帧网格快照:行优先,`cells.len() == cols * rows`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub cells: Vec<SnapCell>,
    pub cursor: Cursor,
}

impl GridSnapshot {
    /// 第 `row` 行的单元格切片(长度 == cols)。
    pub fn row(&self, row: u16) -> &[SnapCell] {
        let start = row as usize * self.cols as usize;
        &self.cells[start..start + self.cols as usize]
    }
}
```

- [ ] **Step 2: 写调色板 + 解析的失败测试**

写 `crates/mullion-term/src/palette.rs`(先只写文件头 + 测试,`resolve`/表待实现):

```rust
//! xterm-256 默认调色板 + 颜色解析(F10)。
//!
//! 核 alacritty 0.26 源码后确认:`Term::colors()` 返回的 `Colors([Option<Rgb>;269])`
//! 默认**全为 None**,只装 OSC-4 运行时覆盖,不含任何默认 RGB。默认 RGB 归我们所有 ——
//! 这里就是那张表。解析优先用覆盖,否则查默认表。这层是纯函数,是干净的可测面。

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, NamedColor, Rgb as AnsiRgb};

use crate::snapshot::Rgb;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_red_resolves_to_ansi_red() {
        let colors = Colors::default();
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Red), &colors),
            Rgb::new(205, 0, 0)
        );
    }

    #[test]
    fn indexed_matches_named_for_first_16() {
        let colors = Colors::default();
        assert_eq!(resolve(AnsiColor::Indexed(1), &colors), Rgb::new(205, 0, 0));
    }

    #[test]
    fn cube_pure_red_index_196() {
        // 6×6×6 立方:196-16=180 → r=5,g=0,b=0 → (255,0,0)。
        assert_eq!(indexed_default(196), Rgb::new(255, 0, 0));
    }

    #[test]
    fn grayscale_index_232_is_dark_gray() {
        // 灰阶:(232-232)*10+8 = 8。
        assert_eq!(indexed_default(232), Rgb::new(8, 8, 8));
    }

    #[test]
    fn spec_passes_through() {
        let colors = Colors::default();
        assert_eq!(
            resolve(AnsiColor::Spec(AnsiRgb { r: 1, g: 2, b: 3 }), &colors),
            Rgb::new(1, 2, 3)
        );
    }

    #[test]
    fn osc_override_wins_over_default() {
        let mut colors = Colors::default();
        colors[NamedColor::Red] = Some(AnsiRgb { r: 10, g: 20, b: 30 });
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Red), &colors),
            Rgb::new(10, 20, 30)
        );
    }

    #[test]
    fn default_fg_bg_are_distinct() {
        let colors = Colors::default();
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Background), &colors),
            DEFAULT_BG
        );
        assert_eq!(
            resolve(AnsiColor::Named(NamedColor::Foreground), &colors),
            DEFAULT_FG
        );
    }
}
```

在 `crates/mullion-term/src/lib.rs` 注册模块(放在现有 `pub mod` 之间,保持字母序即可):

```rust
pub mod palette;
pub mod snapshot;
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-term palette 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function resolve` / `indexed_default` / `DEFAULT_BG`。

- [ ] **Step 4: 实现调色板 + 解析**

在 `crates/mullion-term/src/palette.rs` 的 `use` 之后、`#[cfg(test)]` 之前插入:

```rust
/// 16 个标准 ANSI 颜色(经典 xterm 默认值)。索引 0..16。
const ANSI16: [Rgb; 16] = [
    Rgb::new(0, 0, 0),       // 0  black
    Rgb::new(205, 0, 0),     // 1  red
    Rgb::new(0, 205, 0),     // 2  green
    Rgb::new(205, 205, 0),   // 3  yellow
    Rgb::new(0, 0, 238),     // 4  blue
    Rgb::new(205, 0, 205),   // 5  magenta
    Rgb::new(0, 205, 205),   // 6  cyan
    Rgb::new(229, 229, 229), // 7  white
    Rgb::new(127, 127, 127), // 8  bright black
    Rgb::new(255, 0, 0),     // 9  bright red
    Rgb::new(0, 255, 0),     // 10 bright green
    Rgb::new(255, 255, 0),   // 11 bright yellow
    Rgb::new(92, 92, 255),   // 12 bright blue
    Rgb::new(255, 0, 255),   // 13 bright magenta
    Rgb::new(0, 255, 255),   // 14 bright cyan
    Rgb::new(255, 255, 255), // 15 bright white
];

/// 默认前景 / 背景(无 OSC 覆盖时)。
pub const DEFAULT_FG: Rgb = Rgb::new(0xcc, 0xcc, 0xcc);
pub const DEFAULT_BG: Rgb = Rgb::new(0x00, 0x00, 0x00);

/// 256 色索引 → 默认 RGB(0..16 ANSI;16..232 6×6×6 立方;232..256 灰阶)。
pub fn indexed_default(i: u8) -> Rgb {
    match i {
        0..=15 => ANSI16[i as usize],
        16..=231 => {
            let n = i - 16;
            let comp = |c: u8| -> u8 {
                if c == 0 {
                    0
                } else {
                    c * 40 + 55
                }
            };
            Rgb::new(comp(n / 36), comp((n / 6) % 6), comp(n % 6))
        }
        232..=255 => {
            let v = (i - 232) * 10 + 8;
            Rgb::new(v, v, v)
        }
    }
}

fn from_ansi(rgb: AnsiRgb) -> Rgb {
    Rgb::new(rgb.r, rgb.g, rgb.b)
}

fn named_default(named: NamedColor) -> Rgb {
    match named as usize {
        i @ 0..=15 => ANSI16[i],
        256 => DEFAULT_FG, // Foreground
        257 => DEFAULT_BG, // Background
        _ => DEFAULT_FG,   // Bright*/Dim*/Cursor 等 MVP 先落默认前景
    }
}

/// 把一个单元格颜色解析成具体 RGB:OSC 覆盖优先,否则用默认表。
pub fn resolve(color: AnsiColor, colors: &Colors) -> Rgb {
    match color {
        AnsiColor::Spec(rgb) => from_ansi(rgb),
        AnsiColor::Indexed(i) => match colors[i as usize] {
            Some(over) => from_ansi(over),
            None => indexed_default(i),
        },
        AnsiColor::Named(named) => match colors[named] {
            Some(over) => from_ansi(over),
            None => named_default(named),
        },
    }
}
```

> 若 `colors[named]` 报 `Index<NamedColor>` 未实现,核 `alacritty_terminal/src/term/color.rs`(已确认存在 `impl Index<NamedColor> for Colors`);`colors[i as usize]` 走 `Index<usize>`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-term palette 2>&1 | tail -20`
Expected: 7 个测试全 PASS。

- [ ] **Step 6: clippy + fmt + 提交**

```bash
cargo clippy -p mullion-term --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-term/src/snapshot.rs crates/mullion-term/src/palette.rs crates/mullion-term/src/lib.rs
git commit -m "feat(term): 渲染快照数据类型 + xterm-256 调色板解析 (F10)"
```

---

## Task 2: term — `Emulator::snapshot()` + `resize()`

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs`

- [ ] **Step 1: 写快照/尺寸的失败测试**

在 `crates/mullion-term/src/emulator.rs` 的 `#[cfg(test)] mod tests` 里,`pty_write_is_collected` 之后追加:

```rust
    use crate::snapshot::Rgb;

    #[test]
    fn snapshot_has_chars_and_dims() {
        let mut emu = Emulator::new(10, 3);
        emu.feed(b"Hi");
        let snap = emu.snapshot();
        assert_eq!((snap.cols, snap.rows), (10, 3));
        assert_eq!(snap.row(0)[0].ch, 'H');
        assert_eq!(snap.row(0)[1].ch, 'i');
    }

    #[test]
    fn snapshot_sgr_red_sets_fg() {
        let mut emu = Emulator::new(4, 1);
        emu.feed(b"\x1b[31mR");
        let snap = emu.snapshot();
        assert_eq!(snap.row(0)[0].fg, Rgb::new(205, 0, 0), "SGR 31 应解析成我们表里的红");
    }

    #[test]
    fn snapshot_cjk_is_double_width_with_spacer() {
        // F16:CJK 宽字符占两格,右半是 spacer,渲染跳过。
        let mut emu = Emulator::new(6, 1);
        emu.feed("中".as_bytes());
        let snap = emu.snapshot();
        assert_eq!(snap.row(0)[0].ch, '中');
        assert_eq!(snap.row(0)[0].width, 2, "宽字符必须占两格(F16)");
        assert!(snap.row(0)[1].spacer, "宽字符右侧必须是 spacer");
    }

    #[test]
    fn resize_changes_dims() {
        let mut emu = Emulator::new(10, 3);
        emu.resize(20, 5);
        let snap = emu.snapshot();
        assert_eq!((snap.cols, snap.rows), (20, 5));
    }

    #[test]
    fn cursor_starts_top_left() {
        let emu = Emulator::new(8, 4);
        let snap = emu.snapshot();
        assert_eq!((snap.cursor.row, snap.cursor.col), (0, 0));
        assert!(snap.cursor.visible);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term emulator 2>&1 | tail -20`
Expected: 编译失败 —— `no method named snapshot` / `resize`。

- [ ] **Step 3: 实现 snapshot + resize**

在 `crates/mullion-term/src/emulator.rs` 顶部 `use` 区补:

```rust
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;

use crate::palette;
use crate::snapshot::{Cursor, GridSnapshot, SnapCell};
```

在 `impl Emulator` 里(`take_pty_writes` 之后)加:

```rust
    /// 当前屏面的渲染快照(F10/F16)。颜色解析成具体 RGB,宽字符标 width=2。
    /// MVP 无 scrollback(display_offset 恒 0),直接按屏面行列遍历。
    pub fn snapshot(&self) -> GridSnapshot {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let mut cells = Vec::with_capacity(cols * rows);
        for line in 0..rows {
            let row = &grid[Line(line as i32)];
            for col in 0..cols {
                let cell = &row[Column(col)];
                let flags = cell.flags;
                cells.push(SnapCell {
                    ch: cell.c,
                    fg: palette::resolve(cell.fg, colors),
                    bg: palette::resolve(cell.bg, colors),
                    width: if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 },
                    spacer: flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
                });
            }
        }
        let p = grid.cursor.point;
        GridSnapshot {
            cols: cols as u16,
            rows: rows as u16,
            cells,
            cursor: Cursor {
                row: p.line.0 as u16,
                col: p.column.0 as u16,
                visible: true,
            },
        }
    }

    /// 改变网格尺寸(F34:分屏 reflow / 窗口 resize 时调用)。
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term.resize(GridSize { cols, rows });
    }
```

> `grid.columns()`/`grid.screen_lines()` 来自 `Dimensions` trait —— emulator.rs 顶部已 `use alacritty_terminal::grid::Dimensions;`,无需重复引。若 `grid.cursor` 不可见,核 `alacritty_terminal/src/grid/mod.rs`(已确认 `pub cursor: Cursor<T>`)。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term 2>&1 | tail -20`
Expected: 新增 5 个 + 原有测试全 PASS。

- [ ] **Step 5: clippy + fmt + 提交**

```bash
cargo clippy -p mullion-term --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-term/src/emulator.rs
git commit -m "feat(term): Emulator::snapshot 取网格渲染快照 + resize (F10/F16/F34)"
```

---

## Task 3: app — `grid_size_for` 像素→列行 纯函数

**Files:**
- Create: `crates/mullion-app/src/grid.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写失败测试**

写 `crates/mullion-app/src/grid.rs`:

```rust
//! 由窗口像素尺寸算终端列/行数(F34 前置)。纯函数,可脱离窗口单测。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divides_pixels_by_cell() {
        assert_eq!(grid_size_for(800, 600, 10.0, 20.0), (80, 30));
    }

    #[test]
    fn floors_partial_cells() {
        assert_eq!(grid_size_for(805, 615, 10.0, 20.0), (80, 30));
    }

    #[test]
    fn clamps_to_at_least_one() {
        // 窗口比一个单元格还小时不能返回 0 列(会开出非法 PTY)。
        assert_eq!(grid_size_for(5, 5, 10.0, 20.0), (1, 1));
    }
}
```

在 `crates/mullion-app/src/lib.rs` 加 `pub mod grid;`(与现有 `pub mod` 并列)。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app grid 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function grid_size_for`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/grid.rs` 顶部注释后插入:

```rust
/// 像素尺寸 + 单元格尺寸 → (cols, rows)。向下取整,至少 1×1。
pub fn grid_size_for(px_w: u32, px_h: u32, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let cols = ((px_w as f32 / cell_w).floor() as u32).clamp(1, u16::MAX as u32);
    let rows = ((px_h as f32 / cell_h).floor() as u32).clamp(1, u16::MAX as u32);
    (cols as u16, rows as u16)
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app grid 2>&1 | tail -20`
Expected: 3 个 PASS。

- [ ] **Step 5: fmt + 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-app/src/grid.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): grid_size_for 像素→列行 纯函数 (F34)"
```

---

## Task 4: app — CLI 解析 `user@host -p N -i key`

**Files:**
- Create: `crates/mullion-app/src/cli.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写失败测试**

写 `crates/mullion-app/src/cli.rs`:

```rust
//! 解析类 ssh 命令行:`user@host [-p PORT] [-i KEYPATH]`。
//! 自己写一小段,不引 clap(YAGNI)。F2 的 ssh_config 解析仍在范围外。

use std::path::PathBuf;

use mullion_ssh::config::{AuthMethod, SshConfig};

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_full_target() {
        let cfg = parse_args(&v(&["user@192.0.2.10", "-p", "22", "-i", "/path/to/key.pem"])).unwrap();
        assert_eq!(cfg.user, "testuser");
        assert_eq!(cfg.host, "192.0.2.10");
        assert_eq!(cfg.port, 22);
        assert_eq!(cfg.term, "xterm-256color");
        match cfg.auth {
            AuthMethod::PublicKey { path, passphrase } => {
                assert_eq!(path, PathBuf::from("/path/to/key.pem"));
                assert!(passphrase.is_none());
            }
            _ => panic!("给了 -i 应走 PublicKey"),
        }
    }

    #[test]
    fn defaults_port_22_and_agent_without_key() {
        let cfg = parse_args(&v(&["user@host"])).unwrap();
        assert_eq!(cfg.port, 22);
        assert!(matches!(cfg.auth, AuthMethod::Agent), "无 -i 应回退 ssh-agent");
    }

    #[test]
    fn missing_target_is_error() {
        assert!(parse_args(&v(&["-p", "22"])).is_err());
    }

    #[test]
    fn target_without_at_is_error() {
        assert!(parse_args(&v(&["justhost"])).is_err());
    }

    #[test]
    fn bad_port_is_error() {
        assert!(parse_args(&v(&["u@h", "-p", "notnum"])).is_err());
    }
}
```

在 `crates/mullion-app/src/lib.rs` 加 `pub mod cli;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app cli 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function parse_args`。

- [ ] **Step 3: 实现**

在 `crates/mullion-app/src/cli.rs` 的 `use` 之后插入:

```rust
/// 从参数(不含 argv[0])解析连接配置。cols/rows 先给占位默认,
/// 窗口出来后由 window_change 校正到真实尺寸。
pub fn parse_args(args: &[String]) -> Result<SshConfig, String> {
    let mut target: Option<String> = None;
    let mut port: u16 = 22;
    let mut key: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                i += 1;
                let val = args.get(i).ok_or("-p 缺少端口值")?;
                port = val.parse().map_err(|_| format!("端口非法: {val}"))?;
            }
            "-i" => {
                i += 1;
                let val = args.get(i).ok_or("-i 缺少密钥路径")?;
                key = Some(PathBuf::from(val));
            }
            other if other.starts_with('-') => return Err(format!("未知参数: {other}")),
            other => {
                if target.is_some() {
                    return Err(format!("多余参数: {other}"));
                }
                target = Some(other.to_string());
            }
        }
        i += 1;
    }
    let target = target.ok_or("缺少 user@host")?;
    let (user, host) = target.split_once('@').ok_or("目标须形如 user@host")?;
    if user.is_empty() || host.is_empty() {
        return Err("user 和 host 都不能为空".into());
    }
    let auth = match key {
        Some(path) => AuthMethod::PublicKey {
            path,
            passphrase: None,
        },
        None => AuthMethod::Agent,
    };
    Ok(SshConfig {
        host: host.to_string(),
        port,
        user: user.to_string(),
        auth,
        cols: 80,
        rows: 24,
        term: "xterm-256color".to_string(),
    })
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app cli 2>&1 | tail -20`
Expected: 5 个 PASS。

- [ ] **Step 5: fmt + 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-app/src/cli.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 类 ssh CLI 解析 user@host -p -i → SshConfig"
```

---

## Task 5: app — `text.rs` 行→带色 span 映射(纯部分)

**Files:**
- Create: `crates/mullion-app/src/text.rs`
- Modify: `crates/mullion-app/src/lib.rs`

> 本任务只做**可测的纯映射**(把一行单元格切成 glyphon 富文本段)。glyphon 的 FontSystem/Atlas/Renderer 胶水在 Task 8 一并写(不可测)。

- [ ] **Step 1: 写失败测试**

写 `crates/mullion-app/src/text.rs`:

```rust
//! 文字前景层:把网格快照映射成 glyphon 富文本(纯,可测),
//! 以及 glyphon 资源封装(GPU 胶水,见 Task 8)。

use glyphon::Color;
use mullion_term::snapshot::{Rgb, SnapCell};

/// term 的 Rgb → glyphon 颜色。
pub fn to_color(c: Rgb) -> Color {
    Color::rgb(c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(ch: char, fg: Rgb, spacer: bool) -> SnapCell {
        SnapCell {
            ch,
            fg,
            bg: Rgb::new(0, 0, 0),
            width: if ch == '中' { 2 } else { 1 },
            spacer,
        }
    }

    #[test]
    fn splits_spans_by_fg() {
        let white = Rgb::new(0xcc, 0xcc, 0xcc);
        let red = Rgb::new(205, 0, 0);
        let row = [cell('a', white, false), cell('b', red, false)];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, "a");
        assert_eq!(spans[1].0, "b");
        assert_eq!(spans[0].1, to_color(white));
        assert_eq!(spans[1].1, to_color(red));
    }

    #[test]
    fn merges_same_fg_run() {
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row = [cell('a', w, false), cell('b', w, false), cell('c', w, false)];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "abc");
    }

    #[test]
    fn skips_wide_char_spacer() {
        // F16:宽字符右半 spacer 不产生字形;'中' 与后续 'x' 同色应合并成 "中x"。
        let w = Rgb::new(0xcc, 0xcc, 0xcc);
        let row = [cell('中', w, false), cell(' ', w, true), cell('x', w, false)];
        let spans = row_to_spans(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "中x");
    }
}
```

在 `crates/mullion-app/src/lib.rs` 加 `pub mod text;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app text 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function row_to_spans`。

- [ ] **Step 3: 实现纯映射**

在 `text.rs` 的 `to_color` 之后插入:

```rust
/// 把一行单元格切成 (文本, 颜色) 段:连续同前景色合一段,跳过宽字符 spacer。
/// 供 `glyphon::Buffer::set_rich_text` 使用(每段一个 `Attrs` 带 fg 色)。
pub fn row_to_spans(cells: &[SnapCell]) -> Vec<(String, Color)> {
    let mut spans: Vec<(String, Color)> = Vec::new();
    for cell in cells {
        if cell.spacer {
            continue; // 宽字符右半:字形已由左格承载
        }
        let color = to_color(cell.fg);
        match spans.last_mut() {
            Some((s, c)) if *c == color => s.push(cell.ch),
            _ => spans.push((cell.ch.to_string(), color)),
        }
    }
    spans
}
```

> `glyphon::Color` 是 `cosmic_text::Color(pub u32)`,派生 `PartialEq`,`*c == color` 可用。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app text 2>&1 | tail -20`
Expected: 3 个 PASS。

- [ ] **Step 5: fmt + 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-app/src/text.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 行→带色 span 映射,跳过 CJK spacer (F16)"
```

---

## Task 6: app — `gpu.rs` 色块生成 `quads_for`(纯部分)

**Files:**
- Create: `crates/mullion-app/src/gpu.rs`
- Modify: `crates/mullion-app/src/lib.rs`

> 本任务只做**可测的纯函数**(哪些格要画色块)。wgpu 表面/管线胶水在 Task 8 写(不可测)。

- [ ] **Step 1: 写失败测试**

写 `crates/mullion-app/src/gpu.rs`:

```rust
//! GPU 层:背景/光标色块生成(纯,可测)+ wgpu 表面与色块管线(GPU 胶水,见 Task 8)。

use mullion_term::snapshot::{GridSnapshot, Rgb};

/// 一个实心色块(背景 / 光标),像素坐标(左上原点)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [u8; 3],
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_term::snapshot::{Cursor, SnapCell};

    fn snap_1x1(bg: Rgb) -> GridSnapshot {
        GridSnapshot {
            cols: 1,
            rows: 1,
            cells: vec![SnapCell {
                ch: ' ',
                fg: Rgb::new(0xcc, 0xcc, 0xcc),
                bg,
                width: 1,
                spacer: false,
            }],
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: false,
            },
        }
    }

    #[test]
    fn default_bg_cell_makes_no_quad() {
        let snap = snap_1x1(Rgb::new(0, 0, 0)); // == DEFAULT_BG
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
        assert!(quads.is_empty(), "默认背景不该产生色块(省 GPU)");
    }

    #[test]
    fn colored_bg_cell_makes_quad_at_pixel() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
        assert_eq!(quads.len(), 1);
        assert_eq!(
            quads[0],
            Quad { x: 0.0, y: 0.0, w: 10.0, h: 20.0, color: [205, 0, 0] }
        );
    }

    #[test]
    fn visible_cursor_adds_block_quad() {
        let mut snap = snap_1x1(Rgb::new(0, 0, 0));
        snap.cursor.visible = true;
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
        assert_eq!(quads.len(), 1, "仅光标块(默认背景无块)");
        assert_eq!(quads[0].w, 10.0);
    }
}
```

在 `crates/mullion-app/src/lib.rs` 加 `pub mod gpu;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app gpu 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function quads_for`。

- [ ] **Step 3: 实现**

在 `gpu.rs` 的 `Quad` 定义之后插入:

```rust
/// 从快照生成需要画的色块:bg ≠ 默认 的格 + 可见光标(块状)。纯函数,可单测。
pub fn quads_for(snap: &GridSnapshot, cell_w: f32, cell_h: f32, default_bg: Rgb) -> Vec<Quad> {
    let mut quads = Vec::new();
    for row in 0..snap.rows {
        for (col, cell) in snap.row(row).iter().enumerate() {
            if cell.spacer || cell.bg == default_bg {
                continue;
            }
            quads.push(Quad {
                x: col as f32 * cell_w,
                y: row as f32 * cell_h,
                w: cell.width.max(1) as f32 * cell_w,
                h: cell_h,
                color: [cell.bg.r, cell.bg.g, cell.bg.b],
            });
        }
    }
    if snap.cursor.visible {
        quads.push(Quad {
            x: snap.cursor.col as f32 * cell_w,
            y: snap.cursor.row as f32 * cell_h,
            w: cell_w,
            h: cell_h,
            color: [0xcc, 0xcc, 0xcc], // MVP 块状光标用默认前景色
        });
    }
    quads
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app gpu 2>&1 | tail -20`
Expected: 3 个 PASS。

- [ ] **Step 5: fmt + 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-app/src/gpu.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): quads_for 背景/光标色块生成 (纯函数)"
```

---

## Task 7: app — `input.rs` winit 键 → keymap 键(纯)

**Files:**
- Create: `crates/mullion-app/src/input.rs`
- Modify: `crates/mullion-app/src/lib.rs`

- [ ] **Step 1: 写失败测试**

写 `crates/mullion-app/src/input.rs`:

```rust
//! winit 键盘事件 → term keymap 的 (Key, Mods)。纯映射,可脱离窗口单测。
//! 编码本身(含 T6 Shift+Enter)在 `mullion_term::keymap::encode_key`,这里只做翻译。

use mullion_term::keymap::{Key, Mods};
use winit::event::KeyEvent;
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::ElementState;
    use winit::keyboard::{Key as WKey, NamedKey, PhysicalKey};

    // 构造一个最小 KeyEvent 用于测试翻译。
    fn ev(logical: WKey) -> KeyEvent {
        KeyEvent {
            physical_key: PhysicalKey::Unidentified(
                winit::keyboard::NativeKeyCode::Unidentified,
            ),
            logical_key: logical,
            text: None,
            location: winit::keyboard::KeyLocation::Standard,
            state: ElementState::Pressed,
            repeat: false,
            platform_specific: Default::default(),
        }
    }

    #[test]
    fn enter_maps_to_key_enter() {
        let (key, mods) = translate_key(&ev(WKey::Named(NamedKey::Enter)), ModifiersState::SHIFT).unwrap();
        assert_eq!(key, Key::Enter);
        assert!(mods.shift);
    }

    #[test]
    fn char_maps_to_key_char() {
        let (key, _) = translate_key(&ev(WKey::Character("a".into())), ModifiersState::empty()).unwrap();
        assert_eq!(key, Key::Char('a'));
    }

    #[test]
    fn multichar_ime_is_ignored() {
        // 多字符(输入法合成)MVP 先不当按键处理,交给后续 IME 支持。
        assert!(translate_key(&ev(WKey::Character("ab".into())), ModifiersState::empty()).is_none());
    }
}
```

在 `crates/mullion-app/src/lib.rs` 加 `pub mod input;`。

> `KeyEvent` 的字段名/`platform_specific` 构造在无头测试里可能随平台略变。跑测试若某字段不匹配,核 `winit-0.30.13/src/event.rs` 的 `pub struct KeyEvent` 实际字段并对齐(已确认含 `logical_key: keyboard::Key`、`state: ElementState`)。若 `KeyEvent` 无法在测试中直接构造(字段私有),退化为直接测一个 `fn translate_logical(logical: &WKey, mods: ModifiersState)` 纯函数,`translate_key` 转调它 —— 断言逻辑不变。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app input 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function translate_key`。

- [ ] **Step 3: 实现**

在 `input.rs` 的 `use` 之后插入:

```rust
/// 把一次 winit 按键翻译成 term 的 (Key, Mods);无法映射的键返回 None。
pub fn translate_key(event: &KeyEvent, mods: ModifiersState) -> Option<(Key, Mods)> {
    let m = Mods {
        shift: mods.shift_key(),
        ctrl: mods.control_key(),
        alt: mods.alt_key(),
        sup: mods.super_key(),
    };
    let key = match &event.logical_key {
        WKey::Named(NamedKey::Enter) => Key::Enter,
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app input 2>&1 | tail -20`
Expected: 3 个 PASS。

- [ ] **Step 5: fmt + 提交**

```bash
cargo clippy -p mullion-app --all-targets -- -D warnings && cargo fmt --check
git add crates/mullion-app/src/input.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): winit 键→keymap 翻译 translate_key (纯, F13/F14 前置)"
```

---

## Task 8: app — 端到端接线(GPU 胶水 + 事件循环)

> **本任务大部分不可自动验证。** wgpu/glyphon 的每个调用**必须按锁定源码核对**(路径见计划头);守护 = `cargo build -p mullion-app` 通过 + `cargo run` 能起窗口。字形/颜色/是否不闪/CJK 对齐进 PR 人工验证清单,**不编造通过结论**。整个任务作为一次实现,但按下面小步推进,每步 `cargo build` 保持可编译。

**Files:**
- Modify: `crates/mullion-app/src/text.rs`(加 glyphon 文字层)
- Modify: `crates/mullion-app/src/gpu.rs`(加 wgpu 表面 + 色块管线)
- Rewrite: `crates/mullion-app/src/app.rs`(`App` 实体)
- Rewrite: `crates/mullion-app/src/main.rs`(EventLoop + 连接)

- [ ] **Step 1: wgpu 表面 + 色块管线(`gpu.rs` 追加 `Gpu`)**

在 `gpu.rs` 追加下述 `Gpu`。**核对 wgpu 23 签名**:`Instance::new(InstanceDescriptor)`、`instance.create_surface(window)`、`instance.request_adapter(&RequestAdapterOptions).await`、`adapter.request_device(&DeviceDescriptor, None).await`、`surface.configure(&device, &config)`、`surface.get_current_texture()`。色块用**实例化**绘制:每个 `Quad` 一个实例,顶点着色器把像素矩形 + 屏幕分辨率 uniform 换算到 NDC。

```rust
use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

/// wgpu 表面 + 设备 + 色块管线。GPU 胶水:无单测,守护=编译+起窗口。
pub struct Gpu {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    quad_pipeline: wgpu::RenderPipeline,
    resolution_buf: wgpu::Buffer,
    resolution_bind: wgpu::BindGroup,
}

/// 传给着色器的每实例数据:像素矩形 + 归一化颜色。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadInstance {
    rect: [f32; 4],  // x, y, w, h(像素)
    color: [f32; 4], // r,g,b,1
}

impl Gpu {
    /// 用 `handle`(app 的 tokio 运行时)block_on wgpu 的 async 初始化。
    pub fn new(window: Arc<Window>, handle: &tokio::runtime::Handle) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window.clone()).expect("create_surface");
        let adapter = handle
            .block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))
            .expect("无可用 GPU adapter");
        let (device, queue) = handle
            .block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
            .expect("request_device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo, // vsync,配合帧率闸 T3
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // resolution uniform(vec2<f32>,补齐到 16 字节)。
        let resolution_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("resolution"),
            contents: bytemuck::cast_slice(&[size.width as f32, size.height as f32, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("res-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let resolution_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("res-bind"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: resolution_buf.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad-shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad-layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self { surface, device, queue, config, quad_pipeline, resolution_buf, resolution_bind }
    }

    /// 表面 resize(窗口尺寸变)。
    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
        self.queue.write_buffer(
            &self.resolution_buf,
            0,
            bytemuck::cast_slice(&[w as f32, h as f32, 0.0, 0.0]),
        );
    }

    /// 把色块转成实例缓冲(每帧一次性上传)。
    pub fn quad_instances(&self, quads: &[Quad]) -> wgpu::Buffer {
        let data: Vec<QuadInstance> = quads
            .iter()
            .map(|q| QuadInstance {
                rect: [q.x, q.y, q.w, q.h],
                color: [
                    q.color[0] as f32 / 255.0,
                    q.color[1] as f32 / 255.0,
                    q.color[2] as f32 / 255.0,
                    1.0,
                ],
            })
            .collect();
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-instances"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::VERTEX,
        })
    }

    /// 在已开的 render pass 里画所有色块。
    pub fn draw_quads<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, inst: &'a wgpu::Buffer, n: u32) {
        if n == 0 {
            return;
        }
        pass.set_pipeline(&self.quad_pipeline);
        pass.set_bind_group(0, &self.resolution_bind, &[]);
        pass.set_vertex_buffer(0, inst.slice(..));
        pass.draw(0..4, 0..n);
    }
}

const QUAD_WGSL: &str = r#"
@group(0) @binding(0) var<uniform> resolution: vec4<f32>;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

@vertex
fn vs_main(@builtin(vertex_index) vi: u32,
           @location(0) rect: vec4<f32>,
           @location(1) color: vec4<f32>) -> VsOut {
    // TriangleStrip 四角:(0,0)(1,0)(0,1)(1,1)
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = rect.xy + corner * rect.zw;        // 像素坐标(左上原点)
    let ndc = vec2<f32>(
        px.x / resolution.x * 2.0 - 1.0,
        1.0 - px.y / resolution.y * 2.0,        // y 翻转
    );
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;
```

在 `crates/mullion-app/Cargo.toml` 的 `[dependencies]` 加 `bytemuck = { version = "1", features = ["derive"] }`(wgpu 已传递依赖它,显式声明以便用 derive)。核对 workspace 是否已有 `bytemuck`;有则用 `bytemuck.workspace = true`。

Run: `cargo build -p mullion-app 2>&1 | tail -30` —— 期望编译通过(有 unused 警告正常,后续步骤会用上)。

- [ ] **Step 2: glyphon 文字层(`text.rs` 追加 `TextLayer`)**

在 `text.rs` 追加。**核对 glyphon 0.7 签名**(计划头已列):`FontSystem::new()`、`SwashCache::new()`、`Cache::new(&device)`、`Viewport::new(&device,&cache)`、`TextAtlas::new(&device,&queue,&cache,format)`、`TextRenderer::new(&mut atlas,&device,MultisampleState::default(),None)`、`Buffer::new(&mut fs, Metrics::new(font,line))`、`buffer.set_rich_text(spans, Attrs::new().family(Family::Monospace), Shaping::Advanced)`、`renderer.prepare(&device,&queue,&mut fs,&mut atlas,&viewport,text_areas,&mut cache)`、`renderer.render(&atlas,&viewport,&mut pass)`、`viewport.update(&queue, Resolution{width,height})`。

```rust
use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextArea,
    TextAtlas, TextBounds, TextRenderer, Viewport,
};
use mullion_term::snapshot::GridSnapshot;

/// glyphon 文字资源 + 每行一个 Buffer。GPU 胶水:无单测。
pub struct TextLayer {
    font_system: FontSystem,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    renderer: TextRenderer,
    buffers: Vec<Buffer>, // 每屏面行一个
    pub cell_w: f32,
    pub cell_h: f32,
}

impl TextLayer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, font_px: f32) -> Self {
        let mut font_system = FontSystem::new();
        let swash = SwashCache::new();
        let cache = Cache::new(device);
        let viewport = Viewport::new(device, &cache);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let renderer = TextRenderer::new(&mut atlas, device, wgpu::MultisampleState::default(), None);
        let line_h = (font_px * 1.25).ceil();
        // 用 'M' 的 advance 估等宽单元格宽度。
        let cell_w = measure_cell_w(&mut font_system, font_px, line_h);
        Self {
            font_system,
            swash,
            atlas,
            viewport,
            renderer,
            buffers: Vec::new(),
            cell_w,
            cell_h: line_h,
        }
    }

    /// 每帧:按快照重建各行 Buffer 文本,prepare 上传。
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        snap: &GridSnapshot,
        res: Resolution,
    ) {
        self.viewport.update(queue, res);
        let metrics = Metrics::new(self.cell_h * 0.8, self.cell_h);
        self.buffers.clear();
        for row in 0..snap.rows {
            let spans = row_to_spans(snap.row(row));
            let mut buf = Buffer::new(&mut self.font_system, metrics);
            buf.set_size(&mut self.font_system, Some(res.width as f32), Some(self.cell_h));
            let attrs = Attrs::new().family(Family::Monospace);
            let iter = spans.iter().map(|(s, c)| (s.as_str(), attrs.color(*c)));
            buf.set_rich_text(&mut self.font_system, iter, attrs, Shaping::Advanced);
            buf.shape_until_scroll(&mut self.font_system, false);
            self.buffers.push(buf);
        }
        let cell_h = self.cell_h;
        let areas: Vec<TextArea> = self
            .buffers
            .iter()
            .enumerate()
            .map(|(row, buf)| TextArea {
                buffer: buf,
                left: 0.0,
                top: row as f32 * cell_h,
                scale: 1.0,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: res.width as i32,
                    bottom: res.height as i32,
                },
                default_color: glyphon::Color::rgb(0xcc, 0xcc, 0xcc),
                custom_glyphs: &[],
            })
            .collect();
        self.renderer
            .prepare(device, queue, &mut self.font_system, &mut self.atlas, &self.viewport, areas, &mut self.swash)
            .expect("glyphon prepare");
    }

    pub fn render<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        self.renderer.render(&self.atlas, &self.viewport, pass).expect("glyphon render");
    }
}

/// 用 'M' 估等宽字符宽度。核对 cosmic-text 0.12 的 LayoutRun / glyph 结构后取宽度。
fn measure_cell_w(fs: &mut FontSystem, font_px: f32, line_h: f32) -> f32 {
    let mut buf = Buffer::new(fs, Metrics::new(font_px, line_h));
    buf.set_text(fs, "M", Attrs::new().family(Family::Monospace), Shaping::Advanced);
    buf.shape_until_scroll(fs, false);
    buf.layout_runs()
        .next()
        .and_then(|run| run.glyphs.last().map(|g| g.x + g.w))
        .unwrap_or(font_px * 0.6)
        .max(1.0)
}
```

> `Buffer::set_size` 在 0.12 需 `&mut FontSystem` + 两个 `Option<f32>`(已确认签名 `set_size(font_system, width_opt, height_opt)`)。`layout_runs()`/glyph 字段名(`x`,`w`)按 cosmic-text 0.12 源码核对;取不到就退化 `font_px*0.6`。

Run: `cargo build -p mullion-app 2>&1 | tail -30` —— 期望编译通过。

- [ ] **Step 3: 重写 `app.rs` 为 `App` 实体**

整份替换 `crates/mullion-app/src/app.rs`(保留文件末尾原有 `#[cfg(test)] mod tests` 的 `redraw_is_frame_capped` / `reflow_emits_resize` 两个测试,原样挪到新内容之后)。

```rust
//! App:winit ApplicationHandler<UserEvent>。拥有窗口/GPU/文字层/pane/SSH 会话/运行时,
//! 每帧「排空 rx → feed emu → 回写 PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸。

use std::sync::Arc;
use std::time::Instant;

use mullion_core::layout::PaneId;
use mullion_ssh::session::SshSession;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::frame::FrameLimiter;
use crate::gpu::{quads_for, Gpu};
use crate::pane::Pane;
use crate::render::SyncFramePacer;
use crate::text::TextLayer;
use crate::{grid, input, session_pump};

/// 唤醒重绘的用户事件(ssh io_task 经注入的 wake 回调触发)。
#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    Wake,
}

/// 窗口出现后才建的 GPU 相关状态。
struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    text: TextLayer,
    grid_dims: (u16, u16),
}

pub struct App {
    _runtime: Runtime,
    ssh: SshSession,
    rx: Receiver<Vec<u8>>,
    pane: Pane,
    pacer: SyncFramePacer,
    limiter: FrameLimiter,
    start: Instant,
    mods: ModifiersState,
    kitty: bool,
    active: Option<Active>,
}

impl App {
    pub fn new(runtime: Runtime, ssh: SshSession, rx: Receiver<Vec<u8>>) -> Self {
        Self {
            _runtime: runtime,
            ssh,
            rx,
            pane: Pane::new(PaneId(1), 80, 24),
            pacer: SyncFramePacer::new(),
            limiter: FrameLimiter::new(16), // ~60fps(T3)
            start: Instant::now(),
            mods: ModifiersState::empty(),
            kitty: false, // MVP 未协商 Kitty,走优雅退化(T6)
            active: None,
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("mullion"))
                .expect("create_window"),
        );
        let gpu = Gpu::new(window.clone(), self._runtime.handle());
        let text = TextLayer::new(&gpu.device, &gpu.queue, gpu.config.format, 16.0);
        let size = window.inner_size();
        let (cols, rows) = grid::grid_size_for(size.width, size.height, text.cell_w, text.cell_h);
        self.pane.emulator.resize(cols, rows);
        let _ = self.ssh.resize(cols, rows); // 初始 window_change 校正到真实尺寸(T4)
        self.active = Some(Active { window, gpu, text, grid_dims: (cols, rows) });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        if let Some(a) = &self.active {
            a.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(size) => {
                if let Some(a) = &mut self.active {
                    a.gpu.resize(size.width, size.height);
                    let (cols, rows) =
                        grid::grid_size_for(size.width, size.height, a.text.cell_w, a.text.cell_h);
                    if (cols, rows) != a.grid_dims {
                        a.grid_dims = (cols, rows);
                        self.pane.emulator.resize(cols, rows);
                        let _ = self.ssh.resize(cols, rows); // T4
                    }
                    a.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let Some((key, mods)) = input::translate_key(&event, self.mods) {
                        let bytes = mullion_term::keymap::encode_key(key, mods, self.kitty);
                        let _ = self.ssh.write(bytes);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // 1. 排空 rx(永远做:保 T1 应答流动 + T3 解耦)
                let mut inbound = Vec::new();
                while let Ok(bytes) = self.rx.try_recv() {
                    inbound.push(bytes);
                }
                for b in &inbound {
                    self.pacer.feed(b); // T2:探测同步块
                }
                // 2. feed emu + 回写 PtyWrite(T1 红线)
                let out = session_pump::pump(&mut self.pane.emulator, &inbound);
                if !out.is_empty() {
                    let _ = self.ssh.write(out);
                }
                // 3. present 受帧率(T3)与同步块(T2)双闸
                let now = self.now_ms();
                let ready = self.pacer.should_present();
                if ready && self.limiter.should_present(now) {
                    if let Some(a) = &mut self.active {
                        render_frame(a, &self.pane);
                    }
                    self.limiter.record_present(now);
                    self.pacer.mark_presented();
                } else if ready {
                    // 被帧率挡住:安排下一帧时刻再画(N3 空闲不空转)
                    event_loop.set_control_flow(ControlFlow::WaitUntil(
                        Instant::now() + std::time::Duration::from_millis(16),
                    ));
                    if let Some(a) = &self.active {
                        a.window.request_redraw();
                    }
                }
            }
            _ => {}
        }
    }
}

/// 一帧渲染:背景色块趟 + 文字前景趟。GPU 胶水,无单测。
fn render_frame(a: &mut Active, pane: &Pane) {
    let snap = pane.emulator.snapshot();
    let res = glyphon::Resolution { width: a.gpu.config.width, height: a.gpu.config.height };
    let quads = quads_for(&snap, a.text.cell_w, a.text.cell_h, mullion_term::palette::DEFAULT_BG);
    let inst = a.gpu.quad_instances(&quads);
    a.text.prepare(&a.gpu.device, &a.gpu.queue, &snap, res);

    let frame = match a.gpu.surface.get_current_texture() {
        Ok(f) => f,
        Err(_) => {
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            return;
        }
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = a
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        a.gpu.draw_quads(&mut pass, &inst, quads.len() as u32); // 背景趟
        a.text.render(&mut pass); // 前景趟
    }
    a.gpu.queue.submit(Some(enc.finish()));
    frame.present();
}
```

> `mullion_term::palette::DEFAULT_BG` 在 Task 1 已 `pub const` 导出,app 直接引用。

- [ ] **Step 4: 重写 `main.rs` 接线 EventLoop + 连接**

整份替换 `crates/mullion-app/src/main.rs`:

```rust
//! mullion 入口:解析 CLI → 先建 EventLoop 拿 proxy → block_on 连接 → run_app。
//! 顺序关键:connect 需要 wake=proxy.send_event,proxy 来自 EventLoop,故必须先建循环。

use std::sync::{Arc, Mutex};

use mullion_app::app::{App, UserEvent};
use mullion_app::cli;
use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};
use mullion_ssh::session::connect;
use winit::event_loop::EventLoop;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = match cli::parse_args(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("参数错误: {e}\n用法: mullion user@host [-p PORT] [-i KEYPATH]");
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("建 tokio 运行时");

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("建事件循环");
    let proxy = event_loop.create_proxy();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = proxy.send_event(UserEvent::Wake);
    });

    let policy = Arc::new(TofuAccept::new(Arc::new(Mutex::new(KnownHosts::new()))));
    let (ssh, rx) = match runtime.block_on(connect(&cfg, policy, wake)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("连接失败: {e}");
            std::process::exit(1);
        }
    };

    let mut app = App::new(runtime, ssh, rx);
    event_loop.run_app(&mut app).expect("run_app");
}
```

> 需要 `mullion-app` 既是 bin 又暴露 `app`/`cli` 模块给 `main.rs`。确认 `crates/mullion-app/src/lib.rs` 有 `pub mod app;`,且 `main.rs` 用 `mullion_app::...` 路径(bin 与 lib 同 crate)。若 `[[bin]]` 与 lib 冲突,保持现有 `[[bin]] name="mullion" path="src/main.rs"` 并让 lib crate 名为 `mullion_app`。核对 `connect` 签名:`pub async fn connect(cfg: &SshConfig, policy: Arc<dyn HostKeyPolicy>, wake: Arc<dyn Fn()+Send+Sync>) -> Result<(SshSession, Receiver<Vec<u8>>), ConnectError>`(见 `session.rs`)。

- [ ] **Step 5: 全量编译 + clippy + fmt**

Run:
```bash
cargo build --workspace 2>&1 | tail -30
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30
cargo fmt --check
```
Expected: 编译通过、clippy 无输出、fmt 干净。**逐一按锁定源码核对报错处的 wgpu/glyphon/winit 真实签名**(计划头已列);同一处连续改两次没过,按 ops.md 停下来问。

- [ ] **Step 6: 全量测试(确认没弄坏既有守护测试)**

Run: `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`
Expected: 全 `ok`,含 T2(`sync_update_defers_present`)、T3(`redraw_is_frame_capped`)、T4(`reflow_emits_resize`)、T5/T6(keymap)、Task 1–7 新增测试。0 failed。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/ crates/mullion-term/src/lib.rs
git commit -m "feat(app): 端到端接线 winit+wgpu+glyphon,单 pane 跑通远端 tmux (F10/F11/F13/F14/F34)

事件循环每帧 排空rx→feed emu→回写PtyWrite(T1);present 受帧率(T3)
与同步块(T2)双闸;Resized 发 window_change(T4);键盘走 keymap(T6)。
GPU 渲染/是否不闪/CJK 对齐无法无头验证,见 PR 人工验证清单。"
```

---

## Task 9: 人工验证清单(写进 PR 描述,不可自动验)

创建/更新 PR 描述,包含下述**必须人眼确认**项(在 Windows 11 上对真实服务器跑 `cargo run -p mullion-app -- user@192.0.2.10 -p 22 -i /path/to/key.pem`):

- [ ] 窗口能起、能看到远端 shell/tmux 输出。
- [ ] **G1 零可见闪烁**:在 tmux 里跑 Claude Code 全屏 TUI,流式输出时不撕裂、不抖(T2 生效)。
- [ ] 键入(字母、Enter、Ctrl+C)能正确送达并回显。
- [ ] **Shift+Enter** 在 Claude Code 里插入换行而非提交(T6)。
- [ ] 拉伸窗口后远端按新列数重排,不错行(T4 window_change)。
- [ ] **CJK 宽字符**目视占两格、不与背景块错位(F16;已知风险:glyphon 逐行 shape 可能偏移)。
- [ ] 颜色正确(前景/背景/tmux 状态栏)。
- [ ] 光标位置正确。
- [ ] 空闲时 CPU 不飙、风扇不起(T3/N3)。
- [ ] 输入法候选框行为(中文输入,若可测)。

若 CJK 对齐或不闪不达标,记入 issue,评估是否需要 spec.md Q1 的退路(专用 wgpu 逐格字形管线)。

---

## Self-Review(写完计划的自查)

**Spec 覆盖**
- §2 快照法 → Task 1(类型+调色板)、Task 2(snapshot)。✓
- §3 数据流/T1/T2/T3 → Task 8 事件循环(pump+双闸)。✓ ADR-004 wake 注入 → main.rs。✓
- §4 模块划分 → Task 1–8 一一对应。✓
- §5 渲染两趟(背景 quad + 文字 span)+ CJK + 光标 → Task 6(quads_for)、Task 5/8(text)、Task 2(width/spacer)。✓
- §6 连接 UX + 顺序(先 EventLoop 后 connect)→ Task 8 Step 4。✓
- §7 测试策略:可测(调色板/CJK/grid_size/CLI/span/quads/键翻译)=Task 1–7;不可测=Task 8/9 人工清单。✓
- §8 落地顺序(term→GPU→SSH→输入/resize)→ 任务顺序一致。✓ F34 window_change=Task 8 Resized。T4=reflow 已有测试保留。

**类型一致性**:`Rgb`(term::snapshot)贯穿 palette/emulator/text/gpu;`GridSnapshot::row()` 被 text/gpu 复用;`SshSession::{write,resize}`、`connect` 签名对齐 session.rs;`FrameLimiter::{should_present,record_present}`、`SyncFramePacer::{feed,should_present,mark_presented}`、`session_pump::pump`、`keymap::{encode_key,Key,Mods}`、`grid_size_for`、`translate_key`、`quads_for`、`row_to_spans` 全部在任务内定义并被后续任务按同名调用。✓

**占位符扫描**:无 TBD/TODO;GPU 胶水处均给具体代码 + 明确「按锁定源码核对」的核对点,非「自行补全」。(自查中发现的 `DEFAULT_BG_APP` 笔误已改为真名 `DEFAULT_BG`。)
