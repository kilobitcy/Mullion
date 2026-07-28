# 切片 B0 实现计划 —— F17 滚动回溯 + F3 TOFU 指纹持久化

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让用户能滚回看历史输出(含 tmux/alt screen 场景的三档分流),并把 SSH 主机密钥指纹持久化到磁盘、首次与变更时弹窗确认。

**Architecture:** F17 全部落在 `mullion-term`(纯逻辑,可单测)+ `mullion-app` 的事件接线;alacritty 的 scrollback 其实一直在跑,缺的只是「有入口调 `scroll_display`」和「`snapshot()` 按 `display_offset` 偏移」。F3 把 `KnownHosts`(内存)升级成 `mullion-store::known_hosts::KnownHostsFile`(TOML 落盘),并把 `HostKeyPolicy::decide` 改成 async——SSH 握手就地挂起,等 GUI 线程经 oneshot 回答。依赖方向不变:`app → {core, term, ssh, store}`。

**Tech Stack:** Rust / alacritty_terminal 0.26 / vte / russh 0.54(ssh-key fork 0.6.11)/ winit 0.30 / egui 0.32 / toml 0.8 / tokio(oneshot)

**设计来源:** `docs/superpowers/specs/2026-07-28-slice-b0-scrollback-tofu-design.md`(commit `5db5ad8`)

**对 spec 的一处偏离(已确认必要):** spec §4.2 要求弹窗展示密钥算法,但 `HostKeyPolicy::decide(host, fp)` 拿不到算法(`Fingerprint::from_public_key` 丢弃了它)。本计划把 trait 改成三参 `decide(host, algo, fp)`,`algo` 由 `check_server_key` 里的 `key.algorithm().to_string()` 提供。这是拿到算法的最小改动。

---

## File Structure

**mullion-term(F17 主战场,纯逻辑)**
- `crates/mullion-term/src/emulator.rs` —— 修改。`snapshot()` 按 `display_offset` 偏移 + 光标滚出可视区置 `visible=false`;新增 `scroll` / `scroll_to_bottom` / `mode` / `with_history` 与 `DEFAULT_HISTORY`。
- `crates/mullion-term/src/keymap.rs` —— 修改。新增 `WheelAction` + `wheel_action()`(三档分流决策,纯函数)+ `encode_wheel_report()`;`Key` 加 `PageUp` / `PageDown`。
- `crates/mullion-term/src/lib.rs` —— 修改。重导出 `TermMode` / `Scroll`,让 app 不必直接依赖 alacritty。

**mullion-store(F3 持久化)**
- `crates/mullion-store/src/known_hosts.rs` —— **新建**。`HostKeyEntry` / `KnownHostsFile`(load/get/record/save + corrupt 备份)。只做同步 IO,零 UI。
- `crates/mullion-store/src/vault.rs` —— 修改一行:`write_atomic` 改 `pub(crate)` 供新模块复用。
- `crates/mullion-store/src/lib.rs` —— 修改。挂模块 + 导出。

**mullion-ssh(异步策略边界)**
- `crates/mullion-ssh/src/known_hosts.rs` —— 修改。`HostKeyFuture` 类型别名;`decide` 改 async + 加 `algo` 参数;`Fingerprint::{to_ssh_string, parse_ssh}`。
- `crates/mullion-ssh/src/session.rs` —— 修改。`check_server_key` 里 `.await` 并传算法。
- `crates/mullion-ssh/src/error.rs` —— 修改。指纹用 `SHA256:base64` 展示(用户要拿 `ssh-keygen -lf` 核对,hex 没法比)。

**mullion-app(接线 + UI)**
- `crates/mullion-app/src/input.rs` —— 修改。`wheel_lines()` / `cell_at()` 两个纯函数 + `PageUp/PageDown` 映射。
- `crates/mullion-app/src/host_key.rs` —— **新建**。`HostKeyPrompt` + `PromptingPolicy`(只读 known-hosts,记录/落盘交 GUI 线程)。
- `crates/mullion-app/src/ui/host_key.rs` —— **新建**。未知/变更两态弹窗。
- `crates/mullion-app/src/ui/mod.rs` —— 修改。`UiState` 加 `host_key_reply`;`build_ui` 增一个视图参数。
- `crates/mullion-app/src/app.rs` —— 修改。`MouseWheel`/`CursorMoved` 分支、`UserEvent::HostKeyPrompt`、`modal` 表达式、意图施加点。
- `crates/mullion-app/src/main.rs` —— 修改。注入 `KnownHostsFile` 而非内存 `KnownHosts`。
- `crates/mullion-app/src/lib.rs` —— 修改。挂 `host_key` 模块。

**文档**
- `docs/gui-render-gotchas.md` —— 追加 F17/F3 两条坑。

## 两件「不要动」的事

写代码前先记住,免得顺手加戏(spec §3.5):

1. **新输出到达时保持滚动位置,是 alacritty 内建的**(`grid/mod.rs:267`:`display_offset != 0`
   时自动加偏移)。**什么都不用做,也不要自作聪明去补**——手动补一遍会双倍偏移,
   表现为「一边看历史一边被往上顶」。
2. **不得放松 `shell/window_state.rs` 的最小化防护**。它挡的是「最小化时 `Resized(0,0)`
   触发 resize → 带 scrollback 的 primary grid 按 1 列 reflow → 历史被 truncate 永久碾平」。
   本切片之前无可见 scrollback,那是空防护;**从这个切片起它才真正有东西可防**。

---

## Phase A —— F17 滚动回溯

### Task 1: snapshot 跟随 display_offset + 滚动入口

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs:9-19`(imports)、`:94-135`(`snapshot`)、`:137-140`(新增方法)
- Test: `crates/mullion-term/src/emulator.rs`(文件内 `mod tests`)

- [ ] **Step 1: 写失败测试**

在 `crates/mullion-term/src/emulator.rs` 的 `mod tests` 里,`use crate::snapshot::Rgb;` 下面加一个取行文本的助手,并追加两个测试(放在 `cursor_starts_top_left` 之后):

```rust
    /// 取快照某一行的可见文本(去掉行尾填充空格),断言可读性好很多。
    fn row_text(snap: &crate::snapshot::GridSnapshot, row: usize) -> String {
        snap.row(row)
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn snapshot_follows_display_offset() {
        // F17:2 行屏面喂 3 行 → "one" 被推进 scrollback,可视区是 two/three。
        // 回溯一行后首行必须变成 "one"——不按 display_offset 偏移的话
        // 「数据滚了、画面不动」,滚轮看起来完全没反应。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(row_text(&emu.snapshot(), 0), "two");

        emu.scroll(Scroll::Delta(1));
        assert_eq!(row_text(&emu.snapshot(), 0), "one");
        assert_eq!(row_text(&emu.snapshot(), 1), "two");

        emu.scroll_to_bottom();
        assert_eq!(row_text(&emu.snapshot(), 0), "two", "回底后应重新贴最新输出");
    }

    #[test]
    fn cursor_hidden_when_scrolled_out_of_viewport() {
        // F17:光标随内容滚出可视区后必须停画,否则会被钉在边缘行上闪。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"one\r\ntwo\r\nthree");
        assert!(emu.snapshot().cursor.visible);

        emu.scroll(Scroll::Delta(1));
        assert!(
            !emu.snapshot().cursor.visible,
            "光标已滚出可视区,仍 visible 会画在错误的行上"
        );

        emu.scroll_to_bottom();
        assert!(emu.snapshot().cursor.visible, "回底后光标应恢复");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term 2>&1 | tail -20`
Expected: 编译失败 —— `no method named 'scroll' found` / `cannot find value 'Scroll' in this scope`。

- [ ] **Step 3: 最小实现**

3a. `crates/mullion-term/src/emulator.rs` 顶部 import 改两行:

```rust
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, Term, TermMode};
```

3b. 把 `snapshot()` 的文档注释与函数体换成(替换 `:94-135` 整段):

```rust
    /// 当前**可视区**的渲染快照(F10/F16/F17)。颜色解析成具体 RGB,宽字符标 width=2。
    ///
    /// F17 陷阱:`grid[Line(i)]` 的行号**不含** `display_offset`,回溯时必须自己减掉,
    /// 否则数据滚了画面不动。光标滚出可视区时置 `visible=false`——下游 `quads_for`
    /// 只在 `visible` 时画光标,否则光标会被钉在边缘行。
    pub fn snapshot(&self) -> GridSnapshot {
        let grid = self.term.grid();
        let colors = self.term.colors();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let offset = grid.display_offset() as i32;
        let mut cells = Vec::with_capacity(cols * rows);
        for line in 0..rows {
            let row = &grid[Line(line as i32 - offset)];
            for col in 0..cols {
                let cell = &row[Column(col)];
                let flags = cell.flags;
                cells.push(SnapCell {
                    ch: cell.c,
                    fg: palette::resolve(cell.fg, colors),
                    bg: palette::resolve(cell.bg, colors),
                    width: if flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    },
                    // 只有 WIDE_CHAR_SPACER(宽字符右半)才跳过渲染;LEADING_WIDE_CHAR_SPACER
                    // 是行尾放不下整体换行时插入的独立空白占位格,左边不是宽字,
                    // 当普通空格处理——标成 spacer 会让下游静默漏画这一列的背景。
                    spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
                });
            }
        }
        let p = grid.cursor.point;
        // 光标行是「相对屏面」的,加上 offset 才是「相对可视区」。
        let cursor_row = p.line.0 + offset;
        GridSnapshot {
            cols: cols as u16,
            rows: rows as u16,
            cells,
            cursor: Cursor {
                row: cursor_row.max(0) as u16,
                col: p.column.0 as u16,
                // MVP 未接 DECTCEM(`\x1b[?25l`/`\x1b[?25h`)光标隐藏/显示;
                // 这里只处理「滚出可视区」这一种不可见(F17)。
                visible: cursor_row >= 0 && (cursor_row as usize) < rows,
            },
        }
    }
```

3c. 在 `resize` 方法之后(`impl Emulator` 内)追加三个方法:

```rust
    /// 滚动可视区(F17 回溯)。`Scroll::Delta(正数)` = 往历史方向(向上)。
    ///
    /// alt screen 下 alacritty 内部**静默无效**(alt grid 的 history 恒 0),
    /// 别指望这里报错——分流必须在上层用 [`Emulator::mode`] 先判(见 `keymap::wheel_action`)。
    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// 跳回最新输出。用户一按普通键就该回到底部,否则「打字了但看不到」。
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// 当前终端模式位(F17:滚轮分流要看 ALT_SCREEN / MOUSE_MODE / ALTERNATE_SCROLL)。
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }
```

3d. 测试模块顶部加 import(在 `use crate::snapshot::Rgb;` 旁):

```rust
    use alacritty_terminal::grid::Scroll;
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term 2>&1 | tail -20`
Expected: `test result: ok.`,全部测试通过(原有 7 个 + 新增 2 个)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/emulator.rs
git commit -m "feat(term): snapshot 跟随 display_offset + 滚动入口 (F17)

新增 scroll/scroll_to_bottom/mode。snapshot 按 display_offset 偏移行号,
光标滚出可视区置 visible=false。守护测试:
emulator::tests::snapshot_follows_display_offset、
emulator::tests::cursor_hidden_when_scrolled_out_of_viewport。"
```

---

### Task 2: 可配置 scrollback 深度 + 重导出

**Files:**
- Modify: `crates/mullion-term/src/emulator.rs:39-56`(`GridSize` 注释)、`:65-76`(`new`)
- Modify: `crates/mullion-term/src/lib.rs`
- Test: `crates/mullion-term/src/emulator.rs`(文件内 `mod tests`)

- [ ] **Step 1: 写失败测试**

追加到 `mod tests`:

```rust
    #[test]
    fn scrollback_holds_configured_lines() {
        // F17:history=3 时最多回溯 3 行。喂 10 行后滚到顶,首行应是 L6
        // (可视区 2 行 = L8/L9,历史 3 行 = L5/L6/L7 → 顶端可视首行 L6)。
        let mut emu = Emulator::with_history(10, 2, 3);
        for i in 0..10 {
            emu.feed(format!("L{i}\r\n").as_bytes());
        }
        emu.scroll(Scroll::Top);
        assert_eq!(row_text(&emu.snapshot(), 0), "L6");
    }

    #[test]
    fn default_history_keeps_a_hundred_lines() {
        // 默认 DEFAULT_HISTORY 足够深:喂 100 行后滚到顶还能看到第一行。
        let mut emu = Emulator::new(10, 2);
        for i in 0..100 {
            emu.feed(format!("L{i}\r\n").as_bytes());
        }
        emu.scroll(Scroll::Top);
        assert_eq!(row_text(&emu.snapshot(), 0), "L0");
    }

    #[test]
    fn alt_screen_has_no_scrollback() {
        // F17 的根本理由:alt screen(tmux/vim)的 grid history 恒 0,
        // alacritty 里改不了。滚轮在 alt 下必须分流(上报/方向键),
        // 指望本地回溯是白费——这条测试把这个事实钉住。
        let mut emu = Emulator::new(10, 2);
        emu.feed(b"\x1b[?1049h"); // 进 alt screen
        emu.feed(b"a\r\nb\r\nc");
        assert_eq!(row_text(&emu.snapshot(), 0), "b");
        emu.scroll(Scroll::Delta(5));
        assert_eq!(
            row_text(&emu.snapshot(), 0),
            "b",
            "alt screen 下 scroll_display 静默无效,画面不该动"
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term 2>&1 | tail -20`
Expected: 编译失败 —— `no function or associated item named 'with_history'`。

- [ ] **Step 3: 最小实现**

3a. `crates/mullion-term/src/emulator.rs`:把 `GridSize` 的文档注释里那句过时的话删掉并改写(`:39-40`):

```rust
/// 仿真器网格尺寸。自己实现 `Dimensions`(alacritty 对 `(usize, usize)` 的实现是
/// `#[cfg(test)]` 私用)。这里只描述**屏面**尺寸;scrollback 深度由
/// `Config::scrolling_history` 决定,与本结构无关(`Term::new` 只读 columns/screen_lines)。
```

3b. 把 `new` 替换成(保留 `new` 作为默认深度的薄封装):

```rust
    /// 默认 scrollback 深度(行)。F17:约 10k 行,和 alacritty 默认一致,
    /// 内存开销可接受(80 列 × 10k 行量级)。
    pub const DEFAULT_HISTORY: usize = 10_000;

    /// 新建 `cols × rows` 的仿真器,scrollback 用 [`Emulator::DEFAULT_HISTORY`]。
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_history(cols, rows, Self::DEFAULT_HISTORY)
    }

    /// 新建仿真器并指定 scrollback 深度(F17 可配置;测试里也用它造浅历史)。
    ///
    /// 注意:`history` 只对 primary grid 生效。alt screen 的 grid 恒 0 行历史,
    /// alacritty 不给改(见 `alt_screen_has_no_scrollback`)。
    pub fn with_history(cols: u16, rows: u16, history: usize) -> Self {
        let collector = PtyWriteCollector::default();
        let dims = GridSize { cols, rows };
        let config = Config {
            scrolling_history: history,
            ..Config::default()
        };
        let term = Term::new(config, &dims, collector.clone());
        Self {
            term,
            parser: Processor::new(),
            collector,
        }
    }
```

3c. `crates/mullion-term/src/lib.rs` 末尾追加:

```rust
/// alacritty 的终端模式位与滚动指令。app 层做滚轮分流要用,经这里重导出,
/// 免得 mullion-app 直接依赖 alacritty_terminal(架构不变量:app 只认识我们四个 crate)。
pub use alacritty_terminal::grid::Scroll;
pub use alacritty_terminal::term::TermMode;
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term 2>&1 | tail -20`
Expected: `test result: ok.`(12 个测试)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/emulator.rs crates/mullion-term/src/lib.rs
git commit -m "feat(term): 可配置 scrollback 深度 + 重导出 TermMode/Scroll (F17)

Emulator::with_history + DEFAULT_HISTORY=10000。守护测试:
emulator::tests::scrollback_holds_configured_lines、
emulator::tests::alt_screen_has_no_scrollback(钉住 alt 无历史这一事实)。"
```

---

### Task 3: 滚轮三档分流(纯决策函数)

**Files:**
- Modify: `crates/mullion-term/src/keymap.rs`(文件末尾 `mouse_should_report` 之后 + 顶部 import)
- Test: `crates/mullion-term/src/keymap.rs`(文件内 `mod tests`)

- [ ] **Step 1: 写失败测试**

先在 `mod tests` 顶部(`use super::*;` 之后)加助手,再追加 6 个测试:

```rust
    fn alt(extra: TermMode) -> TermMode {
        TermMode::ALT_SCREEN | extra
    }

    #[test]
    fn shift_forces_local_scroll_so_user_can_read_history() {
        // T5 同源逃生门:即便对端开了鼠标上报,按住 Shift 也必须走本地回溯,
        // 否则 tmux/Claude Code 全屏下用户永远看不到刷过去的历史。
        let m = alt(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE);
        assert_eq!(
            wheel_action(m, true, 3, (10, 5)),
            WheelAction::LocalScroll { lines: 3 }
        );
    }

    #[test]
    fn primary_screen_wheel_scrolls_locally() {
        // 非 alt screen(普通 shell):滚轮永远是本地回溯。
        assert_eq!(
            wheel_action(TermMode::default(), false, -3, (1, 1)),
            WheelAction::LocalScroll { lines: -3 }
        );
    }

    #[test]
    fn alt_screen_with_mouse_mode_reports_sgr() {
        let m = alt(TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE);
        assert_eq!(
            wheel_action(m, false, 2, (12, 34)),
            WheelAction::Report {
                button: 64,
                col: 12,
                row: 34,
                sgr: true,
                count: 2
            }
        );
        // 向下滚 → button 65。
        assert_eq!(
            wheel_action(m, false, -1, (12, 34)),
            WheelAction::Report {
                button: 65,
                col: 12,
                row: 34,
                sgr: true,
                count: 1
            }
        );
    }

    #[test]
    fn alt_screen_without_mouse_falls_back_to_arrow_keys() {
        // tmux 不开鼠标模式时的常见场景:DECSET 1007 允许把滚轮当方向键,
        // 这样 less/man 之类还能翻页,而不是滚轮完全没反应。
        let m = alt(TermMode::ALTERNATE_SCROLL);
        assert_eq!(
            wheel_action(m, false, 3, (1, 1)),
            WheelAction::ArrowKeys {
                up: true,
                count: 3
            }
        );
    }

    #[test]
    fn alt_screen_without_alternate_scroll_does_nothing() {
        // 对端明确关了 1007 且不收鼠标 → 什么都别发,乱发方向键会误操作。
        assert_eq!(
            wheel_action(alt(TermMode::NONE), false, 3, (1, 1)),
            WheelAction::None
        );
        // 零增量恒 None。
        assert_eq!(
            wheel_action(TermMode::default(), false, 0, (1, 1)),
            WheelAction::None
        );
    }

    #[test]
    fn wheel_report_encoding_matches_sgr_and_x10() {
        assert_eq!(
            encode_wheel_report(64, 12, 34, true),
            b"\x1b[<64;12;34M".to_vec()
        );
        assert_eq!(
            encode_wheel_report(64, 12, 34, false),
            vec![0x1b, b'[', b'M', 96, 44, 66]
        );
        // X10 每个字段最多 223(255-32),超出必须夹紧,否则字节溢出成乱码。
        assert_eq!(
            encode_wheel_report(64, 500, 500, false),
            vec![0x1b, b'[', b'M', 96, 255, 255]
        );
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term keymap 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function 'wheel_action'` / `cannot find type 'WheelAction'`。

- [ ] **Step 3: 最小实现**

3a. `crates/mullion-term/src/keymap.rs` 顶部(文档注释之后、`Mods` 之前)加 import:

```rust
use alacritty_terminal::term::TermMode;
```

3b. 在 `mouse_should_report` 之后追加:

```rust
/// 一次滚轮滚动的处置(F17 §3.2 三档分流)。
///
/// 之所以要分档:alt screen(tmux/vim)在 alacritty 里恒 0 行本地历史,
/// 本地回溯拿不到任何东西,只能把滚轮转成对端认识的东西。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelAction {
    /// 本地回溯 scrollback。`lines` 正数 = 往历史(向上)。
    LocalScroll { lines: i32 },
    /// 上报给对端。`col`/`row` 是 1-based 单元格坐标。
    Report {
        button: u8,
        col: u16,
        row: u16,
        sgr: bool,
        /// 连发次数(一次滚轮可能等于多行)。
        count: u16,
    },
    /// 退化成方向键连发(DECSET 1007 / ALTERNATE_SCROLL)。
    ArrowKeys { up: bool, count: u16 },
    /// 什么都不做。
    None,
}

/// 滚轮分流决策(F17)。纯函数,可脱离窗口单测。
///
/// `lines` 正数 = 向上(往历史);`cell` 是鼠标所在的 1-based 单元格 `(col, row)`。
///
/// 顺序不能换:Shift 恒优先(T5 同源逃生门,用户必须永远能读历史),
/// 然后才轮到 alt screen 判定。
pub fn wheel_action(mode: TermMode, shift: bool, lines: i32, cell: (u16, u16)) -> WheelAction {
    if lines == 0 {
        return WheelAction::None;
    }
    if shift || !mode.contains(TermMode::ALT_SCREEN) {
        return WheelAction::LocalScroll { lines };
    }
    let up = lines > 0;
    let count = lines.unsigned_abs().min(u16::MAX as u32) as u16;
    if mode.intersects(TermMode::MOUSE_MODE) {
        return WheelAction::Report {
            // SGR/X10 通用:滚轮上=64,下=65。
            button: if up { 64 } else { 65 },
            col: cell.0,
            row: cell.1,
            sgr: mode.contains(TermMode::SGR_MOUSE),
            count,
        };
    }
    if mode.contains(TermMode::ALTERNATE_SCROLL) {
        return WheelAction::ArrowKeys { up, count };
    }
    WheelAction::None
}

/// 把一次滚轮上报编码成字节。
///
/// SGR(DECSET 1006):`CSI < b ; col ; row M`,坐标无上限。
/// X10(传统):`CSI M (b+32) (col+32) (row+32)`,每字段一字节,故最大 223;
/// 超出必须夹紧,否则加 32 后溢出成完全不同的坐标。
pub fn encode_wheel_report(button: u8, col: u16, row: u16, sgr: bool) -> Vec<u8> {
    if sgr {
        format!("\x1b[<{button};{col};{row}M").into_bytes()
    } else {
        let clamp = |v: u16| (v.min(223) as u8) + 32;
        vec![0x1b, b'[', b'M', button.saturating_add(32), clamp(col), clamp(row)]
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term keymap 2>&1 | tail -20`
Expected: `test result: ok.`,含 `shift_forces_local_scroll_so_user_can_read_history`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/keymap.rs
git commit -m "feat(term): 滚轮三档分流决策 + 上报编码 (F17)

wheel_action:Shift/非 alt → 本地回溯;alt+鼠标模式 → 上报;
alt+1007 → 方向键;否则不动。守护测试:
keymap::tests::shift_forces_local_scroll_so_user_can_read_history(T5 同源)。"
```

---

### Task 4: PageUp / PageDown 按键

**Files:**
- Modify: `crates/mullion-term/src/keymap.rs:18-36`(`Key`)、`:38-59`(`encode_key`)
- Modify: `crates/mullion-app/src/input.rs:22-46`(`translate_logical`)
- Test: `crates/mullion-term/src/keymap.rs`、`crates/mullion-app/src/input.rs`(各自 `mod tests`)

- [ ] **Step 1: 写失败测试**

`crates/mullion-term/src/keymap.rs` 的 `mod tests` 追加:

```rust
    #[test]
    fn page_keys_are_csi_tilde_sequences() {
        // F17:裸 PageUp/PageDown 照旧转发给对端(tmux/less 自己有翻页);
        // Shift+PageUp 由 app 层截住做本地回溯,不走编码。
        let m = Mods::default();
        assert_eq!(encode_key(Key::PageUp, m, false), b"\x1b[5~".to_vec());
        assert_eq!(encode_key(Key::PageDown, m, false), b"\x1b[6~".to_vec());
    }
```

`crates/mullion-app/src/input.rs` 的 `mod tests` 里,把 `common_named_keys_are_mapped` 末尾追加两行:

```rust
        assert_eq!(m(NamedKey::PageUp), Some(Key::PageUp));
        assert_eq!(m(NamedKey::PageDown), Some(Key::PageDown));
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-term keymap 2>&1 | tail -10`
Expected: 编译失败 —— `no variant named 'PageUp' found for enum 'Key'`。

- [ ] **Step 3: 最小实现**

3a. `crates/mullion-term/src/keymap.rs` 的 `Key` 枚举:把注释里的 `Home/End/PageUp/Down 等后续再扩` 改成 `Home/End 等后续再扩`,并在 `Right,` 之后加两个变体:

```rust
    /// 翻页键。裸键转发对端;Shift+PageUp/Down 由 app 截住做本地回溯(F17)。
    PageUp,
    PageDown,
```

3b. `encode_key` 的 `match key` 里,`Key::Left => ...` 之后加:

```rust
        Key::PageUp => b"\x1b[5~".to_vec(),
        Key::PageDown => b"\x1b[6~".to_vec(),
```

3c. `crates/mullion-app/src/input.rs` 的 `translate_logical`,`WKey::Named(NamedKey::ArrowRight) => Key::Right,` 之后加:

```rust
        WKey::Named(NamedKey::PageUp) => Key::PageUp,
        WKey::Named(NamedKey::PageDown) => Key::PageDown,
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-term keymap 2>&1 | tail -10 && cargo test -p mullion-app input 2>&1 | tail -10`
Expected: 两条命令都 `test result: ok.`。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-term/src/keymap.rs crates/mullion-app/src/input.rs
git commit -m "feat(term): PageUp/PageDown 键码与 winit 映射 (F17)

守护测试:keymap::tests::page_keys_are_csi_tilde_sequences、
input::tests::common_named_keys_are_mapped。"
```

---

### Task 5: app 接线 —— 滚轮/翻页事件

**Files:**
- Modify: `crates/mullion-app/src/input.rs`(顶部 import + 文件末尾新函数 + `mod tests`)
- Modify: `crates/mullion-app/src/app.rs:5-26`(imports)、`:64-108`(`App` 字段)、`:111-140`(`App::new`)、`:510-522`(`KeyboardInput` 分支)、`:504`(新增两个分支)

- [ ] **Step 1: 写失败测试**

`crates/mullion-app/src/input.rs` 的 `mod tests` 追加:

```rust
    use winit::dpi::PhysicalPosition;
    use winit::event::MouseScrollDelta;

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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-app input 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function 'wheel_lines' in this scope`。

- [ ] **Step 3: 最小实现(纯函数部分)**

`crates/mullion-app/src/input.rs` 顶部 import 改成:

```rust
use mullion_term::keymap::{Key, Mods};
use winit::event::{KeyEvent, MouseScrollDelta};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
```

在 `translate_logical` 之后、`mod tests` 之前追加:

```rust
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
/// 不减菜单栏高度:终端文字层就是从窗口原点开始画的(`text.rs` 的
/// `top: row * cell_h`),这里必须用同一套坐标系,否则上报的行号会整体偏移。
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app input 2>&1 | tail -20`
Expected: `test result: ok.`(原有 5 个 + 新增 3 个)。

- [ ] **Step 5: 接进事件循环**

5a. `crates/mullion-app/src/app.rs` 的 import 区,在 `use mullion_ssh::session::SshSession;` 之后加一行:

```rust
use mullion_term::keymap::{Key, Mods, WheelAction};
use mullion_term::Scroll;
```

5b. `App` 结构体末尾(`ui_dirty: bool,` 之后)加字段:

```rust
    /// 指针最近一次的物理像素坐标。`MouseWheel` 事件本身不带坐标,鼠标上报
    /// (F17 alt screen 档)要的 (col,row) 只能靠 `CursorMoved` 记着。
    cursor_px: (f32, f32),
```

5c. `App::new` 的初始化末尾(`ui_dirty: true,` 之后)加:

```rust
            cursor_px: (0.0, 0.0),
```

5d. `WindowEvent::ModifiersChanged(m) => self.mods = m.state(),` 之后插入两个分支:

```rust
            // 指针坐标只在这里更新;滚轮上报要用(F17)。
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_px = (position.x as f32, position.y as f32);
            }
            // F17 滚轮三档分流。决策在 `mullion_term::keymap::wheel_action`(纯函数,
            // 已单测),这里只做 winit 增量→行数、像素→单元格的换算与发送。
            WindowEvent::MouseWheel { delta, .. } => {
                if let (Some(a), Some(conn)) = (self.active.as_ref(), self.conn.as_mut()) {
                    let cell_px = (a.text.cell_w, a.text.cell_h);
                    let lines = input::wheel_lines(delta, cell_px.1);
                    let cell = input::cell_at(self.cursor_px, cell_px, a.grid_dims);
                    let action = mullion_term::keymap::wheel_action(
                        conn.pane.emulator.mode(),
                        self.mods.shift_key(),
                        lines,
                        cell,
                    );
                    match action {
                        WheelAction::LocalScroll { lines } => {
                            conn.pane.emulator.scroll(Scroll::Delta(lines));
                        }
                        WheelAction::Report {
                            button,
                            col,
                            row,
                            sgr,
                            count,
                        } => {
                            let one =
                                mullion_term::keymap::encode_wheel_report(button, col, row, sgr);
                            let mut bytes = Vec::with_capacity(one.len() * count as usize);
                            for _ in 0..count {
                                bytes.extend_from_slice(&one);
                            }
                            let _ = conn.ssh.write(bytes);
                        }
                        WheelAction::ArrowKeys { up, count } => {
                            let key = if up { Key::Up } else { Key::Down };
                            let one =
                                mullion_term::keymap::encode_key(key, Mods::default(), self.kitty);
                            let mut bytes = Vec::with_capacity(one.len() * count as usize);
                            for _ in 0..count {
                                bytes.extend_from_slice(&one);
                            }
                            let _ = conn.ssh.write(bytes);
                        }
                        WheelAction::None => {}
                    }
                }
                // 本地回溯不产生新的终端字节,不标脏这一帧会被 frame_is_dirty 判 Idle
                // 丢掉——滚了但画面不动。
                self.request_ui_redraw();
            }
```

5e. 把 `WindowEvent::KeyboardInput` 分支的内层替换成:

```rust
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let Some((key, mods)) = input::translate_key(&event, self.mods) {
                        // F17:Shift+PageUp/PageDown 是本地翻页,截住不转发对端
                        // (裸 PageUp/PageDown 照旧转发,tmux/less 自己会翻)。
                        if mods.shift && matches!(key, Key::PageUp | Key::PageDown) {
                            let scroll = if matches!(key, Key::PageUp) {
                                Scroll::PageUp
                            } else {
                                Scroll::PageDown
                            };
                            if let Some(conn) = self.conn.as_mut() {
                                conn.pane.emulator.scroll(scroll);
                            }
                            self.request_ui_redraw();
                            return;
                        }
                        let bytes = mullion_term::keymap::encode_key(key, mods, self.kitty);
                        // `let _` 全文件都这样:写/resize 失败(断线等)没有用户提示、
                        // 无重连。断线感知与重连是 S3,后续 spec,这里不做。
                        // launcher 态(conn=None)没有终端可写,按键静默丢弃。
                        if let Some(conn) = self.conn.as_mut() {
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            conn.pane.emulator.scroll_to_bottom();
                            let _ = conn.ssh.write(bytes);
                        }
                    }
                }
            }
```

- [ ] **Step 6: 跑绿**

Run: `cargo test --workspace 2>&1 | grep -nE "test result|FAILED|panicked|^error" | tail -20`
Expected: 全部 `test result: ok.`,无 FAILED/error。

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5`
Expected: 无输出(除 `Finished` 行)。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/input.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 滚轮/Shift+翻页接进事件循环 (F17)

新增 MouseWheel/CursorMoved 分支;wheel_lines/cell_at 两个纯函数已单测。
普通键先 scroll_to_bottom 再发送。守护测试:
input::tests::cell_at_is_one_based_and_clamped、
input::tests::small_pixel_delta_still_scrolls_at_least_one_line。"
```

---

## Phase B —— F3 TOFU 指纹持久化

### Task 6: mullion-store 的 known_hosts 落盘

**Files:**
- Create: `crates/mullion-store/src/known_hosts.rs`
- Modify: `crates/mullion-store/src/vault.rs:189`(`write_atomic` 可见性)
- Modify: `crates/mullion-store/src/lib.rs`

- [ ] **Step 1: 先建模块骨架并挂上(让测试能编译到)**

1a. `crates/mullion-store/src/vault.rs:188-189` 改成:

```rust
/// tmp + rename 原子写:防写到一半崩溃导致两文件 desync。
/// `known_hosts` 模块复用同一实现,故 `pub(crate)`。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
```

1b. `crates/mullion-store/src/lib.rs`:在 `pub mod error;` 之后加 `pub mod known_hosts;`,在 `pub use error::StoreError;` 之后加 `pub use known_hosts::{HostKeyEntry, KnownHostsFile};`。

1c. 新建 `crates/mullion-store/src/known_hosts.rs`,先只写文件头 + 类型声明:

```rust
//! 已知主机密钥指纹的持久化(F3 / TOFU)。
//!
//! 自有 TOML 格式,**不复用** OpenSSH 的 `known_hosts`:我们只需要
//! 「host → (算法, SHA256 指纹)」这一条映射,不做 hashed hostname、通配、证书、
//! `@revoked` 标记。解析别人的格式=继承别人的全部边界情况,得不偿失。
//!
//! 明文存储:指纹是公开信息,泄露无害;加密只会挡住用户自己拿
//! `ssh-keygen -lf` 核对,反而降低安全性。
//!
//! 零 async、零 UI——架构不变量:store 只做同步 IO。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::StoreError;
use crate::vault::write_atomic;

/// 一条主机密钥记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyEntry {
    /// 密钥算法,如 `ssh-ed25519`。只用于弹窗展示与人工核对。
    pub algo: String,
    /// `SHA256:<base64-unpadded>` —— 与 `ssh-keygen -lf` 输出的第二列同格式,
    /// 用户可以直接肉眼比对。
    pub fingerprint: String,
}

/// 磁盘上的文件结构。单独一层是为了将来加字段(如 `version`)不破坏格式。
#[derive(Debug, Default, Serialize, Deserialize)]
struct KnownHostsToml {
    #[serde(default)]
    hosts: BTreeMap<String, HostKeyEntry>,
}

/// 已知主机表 + 它的落盘位置。`Default`(path=None)= 纯内存表,不落盘。
#[derive(Debug, Default)]
pub struct KnownHostsFile {
    path: Option<PathBuf>,
    hosts: BTreeMap<String, HostKeyEntry>,
    /// 读到的文件解析失败。此时 `save()` 必须先把原文件备份成 `.bak` 再写——
    /// 直接覆盖等于把用户全部指纹静默清空,以后每台主机都退化成「首次连接」,
    /// 而这恰好是 MITM 最想要的状态。
    corrupt: bool,
}
```

- [ ] **Step 2: 写失败测试**

在 `crates/mullion-store/src/known_hosts.rs` 末尾追加:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry(fp: &str) -> HostKeyEntry {
        HostKeyEntry {
            algo: "ssh-ed25519".into(),
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn record_then_save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut kh = KnownHostsFile::load(dir.path());
        assert!(kh.get("h1").is_none(), "空目录应是空表");

        kh.record("h1", entry("SHA256:AAAA"));
        kh.save().unwrap();

        let re = KnownHostsFile::load(dir.path());
        assert_eq!(re.get("h1"), Some(&entry("SHA256:AAAA")));
        assert!(!re.is_corrupt());
    }

    #[test]
    fn corrupt_file_is_treated_as_empty_and_backed_up_not_clobbered() {
        // F3:文件坏了不能静默清空——否则用户的全部指纹凭空消失,
        // 每台主机都变回「首次连接」,MITM 再也不会被拦下。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("known_hosts.toml"), b"this is not toml {{{").unwrap();

        let mut kh = KnownHostsFile::load(dir.path());
        assert!(kh.is_corrupt());
        assert!(kh.get("h1").is_none(), "损坏文件当空表,不该凭空造出条目");

        kh.record("h1", entry("SHA256:BBBB"));
        kh.save().unwrap();

        let bak = std::fs::read_to_string(dir.path().join("known_hosts.toml.bak")).unwrap();
        assert!(bak.contains("not toml"), "原文件内容必须完整保留在 .bak 里");
        assert_eq!(
            KnownHostsFile::load(dir.path()).get("h1"),
            Some(&entry("SHA256:BBBB"))
        );
    }

    #[test]
    fn save_leaves_no_tmp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut kh = KnownHostsFile::load(dir.path());
        kh.record("h1", entry("SHA256:CCCC"));
        kh.save().unwrap();
        assert!(
            !dir.path().join("known_hosts.tmp").exists(),
            "原子写的临时文件必须被 rename 掉"
        );
    }

    #[test]
    fn record_returns_the_replaced_entry() {
        let mut kh = KnownHostsFile::default();
        assert_eq!(kh.record("h1", entry("SHA256:AAAA")), None);
        assert_eq!(
            kh.record("h1", entry("SHA256:BBBB")),
            Some(entry("SHA256:AAAA")),
            "覆盖时要能拿回旧值,上层要拿它做变更提示"
        );
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p mullion-store known_hosts 2>&1 | tail -20`
Expected: 编译失败 —— `no function or associated item named 'load' found for struct 'KnownHostsFile'`。

- [ ] **Step 4: 最小实现**

在 `KnownHostsFile` 声明之后、`mod tests` 之前插入:

```rust
impl KnownHostsFile {
    /// 从目录加载 `known_hosts.toml`。文件不存在 = 空表(首次运行,不是错误)。
    ///
    /// 解析失败也不是错误:标记 corrupt + 当空表用,用户还能继续连(会重新弹
    /// TOFU 窗),而不是被一个坏文件锁死在门外。
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("known_hosts.toml");
        let (hosts, corrupt) = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<KnownHostsToml>(&text) {
                Ok(parsed) => (parsed.hosts, false),
                Err(_) => (BTreeMap::new(), true),
            },
            Err(_) => (BTreeMap::new(), false),
        };
        Self {
            path: Some(path),
            hosts,
            corrupt,
        }
    }

    /// 磁盘上的文件是否解析失败(上层据此提示用户)。
    pub fn is_corrupt(&self) -> bool {
        self.corrupt
    }

    pub fn get(&self, host: &str) -> Option<&HostKeyEntry> {
        self.hosts.get(host)
    }

    /// 记进内存表(**不落盘**,落盘由调用方决定时机)。返回被顶掉的旧记录。
    pub fn record(&mut self, host: &str, entry: HostKeyEntry) -> Option<HostKeyEntry> {
        self.hosts.insert(host.to_string(), entry)
    }

    /// 落盘。`path=None`(纯内存表)是 no-op。
    /// corrupt 时先把原文件另存 `known_hosts.toml.bak`,再写新文件。
    pub fn save(&mut self) -> Result<(), StoreError> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        if self.corrupt {
            std::fs::rename(&path, path.with_extension("toml.bak"))?;
            self.corrupt = false;
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&KnownHostsToml {
            hosts: self.hosts.clone(),
        })?;
        write_atomic(&path, text.as_bytes())
    }
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p mullion-store 2>&1 | tail -20`
Expected: `test result: ok.`,含新增 4 个测试。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-store/src/known_hosts.rs crates/mullion-store/src/vault.rs crates/mullion-store/src/lib.rs
git commit -m "feat(store): known_hosts.toml 指纹持久化 (F3)

自有 TOML 格式,明文(指纹是公开信息)。文件损坏时当空表 + 落盘前备份 .bak,
绝不静默清空。守护测试:
known_hosts::tests::corrupt_file_is_treated_as_empty_and_backed_up_not_clobbered。"
```

---

### Task 7: HostKeyPolicy 改 async + 指纹文本互转

**Files:**
- Modify: `crates/mullion-ssh/src/known_hosts.rs:40-46`(`Fingerprint` impl)、`:75-108`(trait + `TofuAccept`)、`:156-178`(测试)
- Modify: `crates/mullion-ssh/src/session.rs`(`check_server_key`)
- Modify: `crates/mullion-ssh/src/error.rs:39-41`、`:51-62`

- [ ] **Step 1: 写失败测试**

1a. `crates/mullion-ssh/src/known_hosts.rs` 的 `mod tests` 里,把 `tofu_records_unknown_then_accepts_same_rejects_changed` 整个替换成 async 版:

```rust
    #[tokio::test]
    async fn tofu_records_unknown_then_accepts_same_rejects_changed() {
        // F3:未知主机首次记录并放行;同指纹再来放行;指纹变更 → Reject(Changed)。
        let known = std::sync::Arc::new(std::sync::Mutex::new(KnownHosts::new()));
        let policy = TofuAccept::new(known);
        let a = fp(b"AAAA");
        let b = fp(b"BBBB");
        assert!(
            matches!(
                policy.decide("h", "ssh-ed25519", &a).await,
                HostKeyDecision::Accept
            ),
            "首次应记录并放行"
        );
        assert!(
            matches!(
                policy.decide("h", "ssh-ed25519", &a).await,
                HostKeyDecision::Accept
            ),
            "同指纹应放行"
        );
        match policy.decide("h", "ssh-ed25519", &b).await {
            HostKeyDecision::Reject(HostKeyOutcome::Changed { expected, got, .. }) => {
                assert_eq!(expected, a);
                assert_eq!(got, b);
            }
            _ => panic!("指纹变更必须 Reject(Changed)(F3 红线)"),
        }
    }
```

1b. 同一 `mod tests` 追加:

```rust
    #[test]
    fn ssh_string_round_trips_and_matches_ssh_keygen_format() {
        // F3:存档里的指纹必须与 `ssh-keygen -lf` 的第二列同格式,
        // 否则用户没法用官方工具核对弹窗里的指纹。
        let k = russh::keys::load_secret_key("tests/fixtures/client_key", None).unwrap();
        let f = Fingerprint::from_public_key(k.public_key());
        let text = f.to_ssh_string();
        assert!(text.starts_with("SHA256:"), "格式不对:{text}");
        assert!(!text.ends_with('='), "OpenSSH 用不带填充的 base64");
        assert_eq!(Fingerprint::parse_ssh(&text), Some(f));
        assert_eq!(Fingerprint::parse_ssh("garbage"), None);
    }
```

1c. `crates/mullion-ssh/src/session.rs` 的 `mod tests` 里追加两条(spec §6 要求的 async
policy 两条路径)。注意 `ClientHandler` 字段虽私有,同模块的 tests 能直接构造:

```rust
    struct AlwaysAccept;
    impl HostKeyPolicy for AlwaysAccept {
        fn decide<'a>(
            &'a self,
            _host: &'a str,
            _algo: &'a str,
            _fp: &'a Fingerprint,
        ) -> HostKeyFuture<'a> {
            Box::pin(std::future::ready(HostKeyDecision::Accept))
        }
    }

    /// 故意在回答前 yield 一次:证明 `check_server_key` 真的 await 得下去,
    /// 而不是只在「策略立刻就绪」的情况下碰巧能跑(弹窗版一定不是立刻就绪)。
    struct RejectAfterYield;
    impl HostKeyPolicy for RejectAfterYield {
        fn decide<'a>(
            &'a self,
            host: &'a str,
            _algo: &'a str,
            fp: &'a Fingerprint,
        ) -> HostKeyFuture<'a> {
            let outcome = HostKeyOutcome::Unknown {
                host: host.to_owned(),
                got: fp.clone(),
            };
            Box::pin(async move {
                tokio::task::yield_now().await;
                HostKeyDecision::Reject(outcome)
            })
        }
    }

    fn handler(policy: Arc<dyn HostKeyPolicy>) -> (ClientHandler, Arc<Mutex<Option<HostKeyOutcome>>>) {
        let outcome = Arc::new(Mutex::new(None));
        (
            ClientHandler {
                host: "h".into(),
                policy,
                outcome: outcome.clone(),
            },
            outcome,
        )
    }

    fn test_pubkey() -> ssh_key::PublicKey {
        russh::keys::load_secret_key("tests/fixtures/client_key", None)
            .unwrap()
            .public_key()
            .clone()
    }

    #[tokio::test]
    async fn policy_accept_completes_handshake() {
        let (mut h, outcome) = handler(Arc::new(AlwaysAccept));
        assert!(h.check_server_key(&test_pubkey()).await.unwrap());
        assert!(outcome.lock().unwrap().is_none(), "放行不该记拒绝原因");
    }

    #[tokio::test]
    async fn policy_reject_aborts_handshake_and_records_reason() {
        // F3:策略拒绝必须让 russh 中止握手(Ok(false)),并把原因留给 establish
        // 翻译成可操作错误——否则用户只看到一句无从下手的传输错误。
        let (mut h, outcome) = handler(Arc::new(RejectAfterYield));
        assert!(!h.check_server_key(&test_pubkey()).await.unwrap());
        assert!(matches!(
            outcome.lock().unwrap().take(),
            Some(HostKeyOutcome::Unknown { .. })
        ));
    }
```

同时把 session.rs 顶部 `:13` 的 import 补上 `HostKeyFuture`:

```rust
use crate::known_hosts::{
    Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyOutcome, HostKeyPolicy,
};
```

(`HostKeyFuture` 只在测试里用到时,clippy 会报 unused import——若如此,改成把
`use crate::known_hosts::HostKeyFuture;` 放进 `mod tests` 内部。)

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p mullion-ssh known_hosts 2>&1 | tail -20`
Expected: 编译失败 —— `no method named 'to_ssh_string'` 以及 `decide` 参数个数不符。

- [ ] **Step 3: 最小实现**

3a. `crates/mullion-ssh/src/known_hosts.rs` 顶部 import 改成:

```rust
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
```

3b. `impl Fingerprint`(`:40-46`)追加两个方法:

```rust
    /// `SHA256:<base64-unpadded>` —— 与 `ssh-keygen -lf` 第二列同格式,用户可肉眼核对。
    /// 非 32 字节(只可能来自测试/伪造)退化成十六进制,保证 UI 永不显示空串。
    pub fn to_ssh_string(&self) -> String {
        match <[u8; 32]>::try_from(self.0.as_slice()) {
            Ok(bytes) => russh::keys::ssh_key::Fingerprint::Sha256(bytes).to_string(),
            Err(_) => self.0.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    /// `SHA256:<base64>` → 指纹字节。解析失败返回 `None`(存档被手改坏)。
    pub fn parse_ssh(s: &str) -> Option<Self> {
        let fp: russh::keys::ssh_key::Fingerprint = s.parse().ok()?;
        Some(Fingerprint(fp.as_bytes().to_vec()))
    }
```

3c. trait 与 `TofuAccept` 的 impl(`:75-108`)替换成:

```rust
/// `HostKeyPolicy::decide` 的返回类型。
///
/// 手写 `Pin<Box<dyn Future>>` 而不是 `async fn`:trait 里的 async fn(AFIT)不是
/// dyn-safe,而 `Arc<dyn HostKeyPolicy>` 正是 ssh 与 app 解耦的关键(ssh 不认识 GUI)。
/// 也不引 `async_trait` 宏——一个方法不值得多一个依赖。
pub type HostKeyFuture<'a> = Pin<Box<dyn Future<Output = HostKeyDecision> + Send + 'a>>;

/// 主机密钥策略。ssh 不弹 UI —— app 注入实现(弹窗版),测试/冒烟注入 TofuAccept。
///
/// 返回 Future:app 的弹窗版要在这里挂起,等 GUI 线程回答(F3)。
pub trait HostKeyPolicy: Send + Sync {
    /// `algo` 形如 `ssh-ed25519`,只用于弹窗展示与人工核对。
    fn decide<'a>(&'a self, host: &'a str, algo: &'a str, fp: &'a Fingerprint)
        -> HostKeyFuture<'a>;
}

/// TOFU 策略:未记录→记录并放行;一致→放行;不一致→拒(Changed)。
/// 冒烟/hermetic 测试用它;app 用 `PromptingPolicy`(弹窗版)。
pub struct TofuAccept {
    known: std::sync::Arc<std::sync::Mutex<KnownHosts>>,
}

impl TofuAccept {
    pub fn new(known: std::sync::Arc<std::sync::Mutex<KnownHosts>>) -> Self {
        Self { known }
    }
}

impl HostKeyPolicy for TofuAccept {
    fn decide<'a>(
        &'a self,
        host: &'a str,
        _algo: &'a str,
        fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        let mut kh = self.known.lock().expect("known-hosts poisoned");
        let decision = match kh.get(host).cloned() {
            None => {
                kh.record(host, fp.clone());
                HostKeyDecision::Accept
            }
            Some(known) if &known == fp => HostKeyDecision::Accept,
            Some(known) => HostKeyDecision::Reject(HostKeyOutcome::Changed {
                host: host.to_owned(),
                expected: known,
                got: fp.clone(),
            }),
        };
        // 本策略不需要等任何人,立刻就绪。
        Box::pin(std::future::ready(decision))
    }
}
```

3d. `crates/mullion-ssh/src/session.rs` 的 `check_server_key` 改成:

```rust
    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let fp = Fingerprint::from_public_key(key);
        // 算法名给上层弹窗展示(用户核对 `ssh-keygen -lf` 时要对得上)。
        let algo = key.algorithm().to_string();
        // 弹窗策略会在这里挂起,等用户回答——sshd 的 LoginGraceTime(默认 120s)
        // 是这里能等多久的上限,超时对端会直接断开。
        match self.policy.decide(&self.host, &algo, &fp).await {
            HostKeyDecision::Accept => Ok(true),
            HostKeyDecision::Reject(o) => {
                *self.outcome.lock().expect("outcome poisoned") = Some(o);
                Ok(false)
            }
        }
    }
```

3e. `crates/mullion-ssh/src/error.rs`:删掉 `fn hex`(`:39-41` 整个函数),并把 Display 里两处 `hex(...)` 换成 `to_ssh_string()`:

```rust
            ConnectError::HostKeyChanged {
                host,
                expected,
                got,
            } => write!(
                f,
                "主机 {host} 的密钥已变更(疑似中间人,已拦截):记录 {} → 收到 {}",
                expected.to_ssh_string(),
                got.to_ssh_string()
            ),
            ConnectError::HostKeyUnknown { host, got } => {
                write!(
                    f,
                    "首次连接 {host},指纹 {} 未记录,需确认(TOFU)",
                    got.to_ssh_string()
                )
            }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-ssh 2>&1 | tail -20`
Expected: `test result: ok.`。若报 `#[tokio::test]` 找不到,确认 `crates/mullion-ssh/Cargo.toml` 里有 `tokio.workspace = true`(workspace 已开 `macros` + `rt-multi-thread`,无需新增依赖)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-ssh/src/known_hosts.rs crates/mullion-ssh/src/session.rs crates/mullion-ssh/src/error.rs
git commit -m "refactor(ssh)!: HostKeyPolicy::decide 改 async 并带算法名 (F3)

握手期可就地挂起等 GUI 回答;新增 Fingerprint::{to_ssh_string,parse_ssh},
错误消息改用 SHA256:base64(与 ssh-keygen -lf 对齐)。守护测试:
known_hosts::tests::ssh_string_round_trips_and_matches_ssh_keygen_format、
known_hosts::tests::tofu_records_unknown_then_accepts_same_rejects_changed、
session::tests::policy_reject_aborts_handshake_and_records_reason。

偏离 spec §4.2 的说明:decide 增加 algo 参数——原签名拿不到密钥算法,
而弹窗必须展示它。"
```

---

### Task 8: app 侧弹窗策略 + 注入持久化指纹表

**Files:**
- Create: `crates/mullion-app/src/host_key.rs`
- Modify: `crates/mullion-app/src/lib.rs`
- Modify: `crates/mullion-app/src/app.rs`(`:11` import、`:31-43` UserEvent、`:80-81` 字段、`:110-137` new、`:235` policy、`:372-424` user_event)
- Modify: `crates/mullion-app/src/main.rs`

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/host_key.rs`,先只写测试模块 + 文件头:

```rust
//! 主机密钥确认(F3):SSH 握手线程 ↔ GUI 线程之间的「挂起—回答」桥。
//!
//! 判断逻辑抽成纯函数 `check`,弹窗与 async 只是它的壳——这样 F3 的红线
//! (指纹变更必须拦下)能在没有窗口、没有 SSH 的情况下单测。

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_store::known_hosts::HostKeyEntry;

    fn entry(fp: &str) -> HostKeyEntry {
        HostKeyEntry {
            algo: "ssh-ed25519".into(),
            fingerprint: fp.into(),
        }
    }

    #[test]
    fn unknown_host_requires_prompt_without_previous() {
        let known = KnownHostsFile::default();
        assert_eq!(
            check(&known, "h", "SHA256:AAAA"),
            HostKeyCheck::NeedsPrompt { previous: None }
        );
    }

    #[test]
    fn known_and_matching_is_trusted_without_prompt() {
        // 每次连都弹窗 = 用户学会闭眼点「接受」,TOFU 就废了。
        let mut known = KnownHostsFile::default();
        known.record("h", entry("SHA256:AAAA"));
        assert_eq!(check(&known, "h", "SHA256:AAAA"), HostKeyCheck::Trusted);
    }

    #[test]
    fn changed_fingerprint_requires_prompt_carrying_previous() {
        // F3 红线:指纹变了必须拦下并把旧指纹一并交给 UI 做对比展示。
        let mut known = KnownHostsFile::default();
        known.record("h", entry("SHA256:AAAA"));
        assert_eq!(
            check(&known, "h", "SHA256:BBBB"),
            HostKeyCheck::NeedsPrompt {
                previous: Some(entry("SHA256:AAAA"))
            }
        );
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

先在 `crates/mullion-app/src/lib.rs` 的 `pub mod grid;` 之后加一行 `pub mod host_key;`,然后:

Run: `cargo test -p mullion-app host_key 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find function 'check' in this scope`。

- [ ] **Step 3: 最小实现**

在 `crates/mullion-app/src/host_key.rs` 的文件头注释之后、`#[cfg(test)]` 之前插入:

```rust
use std::sync::{Arc, Mutex};

use mullion_ssh::known_hosts::{
    Fingerprint, HostKeyDecision, HostKeyFuture, HostKeyOutcome, HostKeyPolicy,
};
use mullion_store::known_hosts::{HostKeyEntry, KnownHostsFile};
use tokio::sync::oneshot;
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

/// 一次待用户回答的主机密钥确认。经 `UserEvent::HostKeyPrompt` 送到 GUI 线程,
/// 用户点完把 bool 从 `reply` 送回,握手线程随即恢复。
pub struct HostKeyPrompt {
    pub host: String,
    /// 形如 `ssh-ed25519`,供用户核对时对上 `ssh-keygen -lf` 的输出。
    pub algo: String,
    /// `SHA256:<base64>`。
    pub fingerprint: String,
    /// 存档里的旧记录;`Some` = 指纹变更(高危,UI 走警告态)。
    pub previous: Option<HostKeyEntry>,
    pub reply: oneshot::Sender<bool>,
}

/// 一次检查的结论。
#[derive(Debug, PartialEq, Eq)]
pub enum HostKeyCheck {
    /// 已记录且一致 —— 直接放行,不打扰用户。
    Trusted,
    /// 需要用户确认。`previous = Some` 表示指纹变更。
    NeedsPrompt { previous: Option<HostKeyEntry> },
}

/// F3 的全部判断逻辑(纯函数,见模块头注释)。
pub fn check(known: &KnownHostsFile, host: &str, fingerprint: &str) -> HostKeyCheck {
    match known.get(host) {
        Some(e) if e.fingerprint == fingerprint => HostKeyCheck::Trusted,
        Some(e) => HostKeyCheck::NeedsPrompt {
            previous: Some(e.clone()),
        },
        None => HostKeyCheck::NeedsPrompt { previous: None },
    }
}

/// 弹窗版主机密钥策略:未知/变更 → 送弹窗事件并在握手里 await 用户回答。
///
/// `proxy` 包 `Mutex` 的原因:winit 0.30 只给 `EventLoopProxy` 实现了 `Send`,
/// 没实现 `Sync`(`platform_impl` 里是个 `Sender`);而 `Arc<dyn HostKeyPolicy>`
/// 要求 `Send + Sync`。`Mutex<T>: Sync where T: Send` 正好补上,锁只在
/// `send_event` 那一瞬持有,**绝不跨 await**。
pub struct PromptingPolicy {
    known: Arc<Mutex<KnownHostsFile>>,
    proxy: Mutex<EventLoopProxy<UserEvent>>,
}

impl PromptingPolicy {
    pub fn new(known: Arc<Mutex<KnownHostsFile>>, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            known,
            proxy: Mutex::new(proxy),
        }
    }
}

impl HostKeyPolicy for PromptingPolicy {
    fn decide<'a>(
        &'a self,
        host: &'a str,
        algo: &'a str,
        fp: &'a Fingerprint,
    ) -> HostKeyFuture<'a> {
        let text = fp.to_ssh_string();
        // 锁的作用域收到最小:后面要 await,std 的 MutexGuard 不能跨 await。
        let outcome_of_check = {
            let known = self.known.lock().expect("known-hosts poisoned");
            check(&known, host, &text)
        };
        let previous = match outcome_of_check {
            HostKeyCheck::Trusted => {
                return Box::pin(std::future::ready(HostKeyDecision::Accept));
            }
            HostKeyCheck::NeedsPrompt { previous } => previous,
        };
        // 用户不同意 / 弹窗送不到 / GUI 先退出时的统一拒绝理由。
        let rejection = match &previous {
            Some(e) => HostKeyOutcome::Changed {
                host: host.to_owned(),
                // 存档里是文本;解析不出(文件被手改坏)只影响错误消息里的展示,
                // 不影响「拒绝」这个判定本身。
                expected: Fingerprint::parse_ssh(&e.fingerprint)
                    .unwrap_or_else(|| Fingerprint(Vec::new())),
                got: fp.clone(),
            },
            None => HostKeyOutcome::Unknown {
                host: host.to_owned(),
                got: fp.clone(),
            },
        };
        let (tx, rx) = oneshot::channel();
        let sent = self
            .proxy
            .lock()
            .expect("proxy poisoned")
            .send_event(UserEvent::HostKeyPrompt(Box::new(HostKeyPrompt {
                host: host.to_owned(),
                algo: algo.to_owned(),
                fingerprint: text,
                previous,
                reply: tx,
            })))
            .is_ok();
        Box::pin(async move {
            // fail-closed:事件循环已关闭(send_event Err)或 sender 被丢弃
            // (rx.await Err,例如 GUI 退出)一律当拒绝。任何「送不到就放行」
            // 的写法都会让 MITM 只要能让 GUI 崩一下就过关。
            if !sent {
                return HostKeyDecision::Reject(rejection);
            }
            match rx.await {
                Ok(true) => HostKeyDecision::Accept,
                _ => HostKeyDecision::Reject(rejection),
            }
        })
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app host_key 2>&1 | tail -20`
Expected: `test result: ok.`,3 个测试通过。

- [ ] **Step 5: 把策略接进事件循环**

5a. `crates/mullion-app/src/app.rs:11`:

```rust
use mullion_ssh::known_hosts::HostKeyPolicy;
```

并在 import 区补上(`use mullion_store::...` 若尚无则新增一行):

```rust
use mullion_store::known_hosts::{HostKeyEntry, KnownHostsFile};
```

5b. `UserEvent`(`:31-43`)在 `KeyPathPicked` 之后加一个变体:

```rust
    /// 主机密钥需要用户确认(F3)。握手线程正挂在 `reply` 上等回答,
    /// **必须**最终发一个 bool 回去或丢弃 sender(丢弃 = 拒绝,fail-closed)。
    /// `Box` 是因为 `HostKeyPrompt` 比其余变体大得多,不装箱会撑大整个枚举。
    HostKeyPrompt(Box<crate::host_key::HostKeyPrompt>),
```

5c. `App` 字段(`:80-81`)把 `tofu` 换成三个字段:

```rust
    /// 已知主机指纹表(F3),对应磁盘 `known_hosts.toml`。SSH 线程只读它做判断;
    /// **写入与落盘只在 GUI 线程的意图施加点做**——store 是同步 IO,不该压在
    /// tokio 线程上,而且失败要能落进 `ui.last_error` 给用户看。
    known_hosts: Arc<Mutex<KnownHostsFile>>,
    /// 正在等用户回答的主机密钥弹窗。`Some` = 弹窗开着、SSH 握手挂起中。
    pending_host_key: Option<Box<crate::host_key::HostKeyPrompt>>,
    /// 弹窗弹出的时刻,用于展示 sshd `LoginGraceTime`(默认 120s)倒计时。
    host_key_since: Option<Instant>,
```

app.rs 顶部若还没 `Mutex`,把 `use std::sync::Arc;` 改成 `use std::sync::{Arc, Mutex};`。

5d. `App::new`(`:111-137`)签名与初始化:

```rust
    pub fn new(
        runtime: Runtime,
        proxy: EventLoopProxy<UserEvent>,
        known_hosts: Arc<Mutex<KnownHostsFile>>,
        initial: Option<SshConfig>,
        cli_direct: bool,
    ) -> Self {
```

字段列表里把 `tofu,` 换成:

```rust
            known_hosts,
            pending_host_key: None,
            host_key_since: None,
```

5e. `spawn_connect`(`:235`)那一行换成:

```rust
        // 每次连接现建一个策略:它只持有两个 Arc/Sender 的克隆,构造成本可忽略,
        // 换来 App 不必长期持有一个 dyn 对象。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
        ));
```

5f. `user_event`(`:406` 的 `KeyPathPicked` 分支之后)加新分支:

```rust
            UserEvent::HostKeyPrompt(prompt) => {
                crate::logx::line(&format!(
                    "主机密钥待确认: {} ({}), 变更={}",
                    prompt.host,
                    prompt.algo,
                    prompt.previous.is_some()
                ));
                // 前一个弹窗还没回答就又来一个(用户连点两次连接):丢掉旧 prompt,
                // 它的 sender 随之析构 → 旧那条握手被拒(fail-closed),不会有
                // 两个窗叠在一起、也不会有连接偷偷放行。
                self.host_key_since = Some(Instant::now());
                self.pending_host_key = Some(prompt);
                self.request_ui_redraw();
            }
```

5g. `crates/mullion-app/src/main.rs`:把 `use mullion_ssh::known_hosts::{KnownHosts, TofuAccept};` 换成 `use mullion_store::KnownHostsFile;`;把建 `tofu` 那一行换成:

```rust
    // F3:主机密钥指纹表,跨进程持久化。拿不到配置目录(极罕见,如无 HOME)时
    // 退化成纯内存表——每次启动都会重新问一遍,但绝不静默放行。
    let known_hosts = Arc::new(Mutex::new(
        match mullion_app::shell::store::config_dir() {
            Some(dir) => KnownHostsFile::load(&dir),
            None => KnownHostsFile::default(),
        },
    ));
    if known_hosts.lock().expect("known-hosts poisoned").is_corrupt() {
        // 不是致命错误(当空表继续跑),但必须留痕:用户会奇怪「为什么又让我确认指纹」。
        mullion_app::logx::line("known_hosts.toml 解析失败,当空表处理;首次保存时会备份为 .bak");
    }
```

并把 `App::new(runtime, proxy, tofu, initial, cli_direct)` 改成 `App::new(runtime, proxy, known_hosts, initial, cli_direct)`。

若 `mullion-app/Cargo.toml` 的 `[dependencies]` 里没有 `mullion-store`,它已经在(shell/store.rs 用着),无需改。

- [ ] **Step 6: 跑绿**

Run: `cargo test --workspace 2>&1 | tail -20 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20`
Expected: 全绿、clippy 无输出。此时弹窗还没画,`pending_host_key` 会被 clippy 判为 `dead_code`——**若报了,先别 `#[allow]`,直接进 Task 9**(Task 9 会用到它)。若想中途保持绿,临时在字段上加 `#[allow(dead_code)]` 并在 Task 9 删掉。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/host_key.rs crates/mullion-app/src/lib.rs crates/mullion-app/src/app.rs crates/mullion-app/src/main.rs
git commit -m "feat(app): 弹窗版 HostKeyPolicy + 持久化指纹表注入 (F3)

判断逻辑抽成纯函数 check(可脱离窗口/SSH 单测);送不到弹窗或 sender 被丢弃
一律拒绝(fail-closed)。守护测试:
host_key::tests::changed_fingerprint_requires_prompt_carrying_previous、
host_key::tests::known_and_matching_is_trusted_without_prompt。"
```

---

### Task 9: 主机密钥确认弹窗(UI)

**Files:**
- Create: `crates/mullion-app/src/ui/host_key.rs`
- Modify: `crates/mullion-app/src/ui/mod.rs:2-3`(挂模块)、`:41-72`(UiState)、`:77-104`(build_ui)
- Modify: `crates/mullion-app/src/app.rs:442`(modal)、`:579-587`(render_frame 调用)、`:691-702` 之后(intent 施加)、`:727-741`(render_frame 签名)

- [ ] **Step 1: 写失败测试**

新建 `crates/mullion-app/src/ui/host_key.rs`,先写文件头 + 测试:

```rust
//! 主机密钥确认弹窗(F3)。未知主机与指纹变更两态,后者是高危警告态。
//!
//! 窗口**没有关闭按钮**:两个动作(接受 / 取消连接)都会给握手线程一个明确答复。
//! 留个 X 会让用户以为「关掉 = 什么都没发生」,而实际上握手正挂着等回答。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_state_is_marked_dangerous_and_defaults_to_cancel() {
        // F3:变更态必须与首次连接态在视觉与默认动作上区分开,否则用户会
        // 用点「首次连接」的肌肉记忆一路点过 MITM 警告。
        let changed = HostKeyView {
            host: "h",
            algo: "ssh-ed25519",
            fingerprint: "SHA256:BBBB",
            previous: Some("SHA256:AAAA"),
            elapsed_secs: 0,
        };
        let first = HostKeyView {
            previous: None,
            ..changed
        };
        assert!(changed.is_changed());
        assert!(!first.is_changed());
        assert_ne!(changed.title(), first.title());
    }

    #[test]
    fn grace_countdown_saturates_at_zero() {
        let v = HostKeyView {
            host: "h",
            algo: "ssh-ed25519",
            fingerprint: "SHA256:BBBB",
            previous: None,
            elapsed_secs: 999,
        };
        // 不能出现负数/回绕的「剩余 18446744073709551615 秒」。
        assert_eq!(v.grace_left_secs(), 0);
        assert_eq!(
            HostKeyView {
                elapsed_secs: 20,
                ..v
            }
            .grace_left_secs(),
            100
        );
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

先在 `crates/mullion-app/src/ui/mod.rs:2` 的 `pub mod chrome;` 之后加 `pub mod host_key;`,然后:

Run: `cargo test -p mullion-app ui::host_key 2>&1 | tail -20`
Expected: 编译失败 —— `cannot find struct 'HostKeyView'`。

- [ ] **Step 3: 最小实现**

在 `crates/mullion-app/src/ui/host_key.rs` 的文件头之后、`#[cfg(test)]` 之前插入:

```rust
/// sshd `LoginGraceTime` 的默认值(秒)。超过它对端会主动断开,弹窗再点也没用,
/// 所以要把倒计时摆在用户面前,而不是让他慢慢核对完发现连接已经没了。
const LOGIN_GRACE_SECS: u64 = 120;

/// 弹窗要展示的只读视图。借用式:`&mut UiState` 与 `&HostKeyPrompt` 是 App 的两个
/// 不相干字段,可以同时借出,不必把 prompt 复制进 UiState 再同步两份状态。
#[derive(Clone, Copy)]
pub struct HostKeyView<'a> {
    pub host: &'a str,
    pub algo: &'a str,
    pub fingerprint: &'a str,
    /// 存档里的旧指纹;`Some` = 变更(高危)。
    pub previous: Option<&'a str>,
    /// 弹窗已开的秒数。
    pub elapsed_secs: u64,
}

impl HostKeyView<'_> {
    pub fn is_changed(&self) -> bool {
        self.previous.is_some()
    }

    pub fn title(&self) -> &'static str {
        if self.is_changed() {
            "⚠ 主机密钥已变更"
        } else {
            "主机密钥确认"
        }
    }

    /// 握手宽限期剩余秒数,饱和到 0。
    pub fn grace_left_secs(&self) -> u64 {
        LOGIN_GRACE_SECS.saturating_sub(self.elapsed_secs)
    }
}

/// 画弹窗。用户做出选择时把 `Some(accept)` 写进 `reply`,由 app.rs 事后施加
/// (记录+落盘+回送给握手线程)——egui 闭包里借不到 `&mut App`,与会话管理器同构。
pub fn show(ctx: &egui::Context, view: &HostKeyView<'_>, reply: &mut Option<bool>) {
    // 倒计时要每秒走一格。走 egui 的 repaint_delay 通道,由 app.rs 按
    // T3/T7 的 next_frame_at/WaitUntil 排期,不绕开帧率闸。
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    egui::Window::new(view.title())
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if view.is_changed() {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 40, 40),
                    "此主机的密钥与上次记录的不同。可能是服务器重装/换了密钥,\
                     也可能是有人在中间冒充它。确认之前不要输入任何密码。",
                );
            } else {
                ui.label("首次连接此主机,尚无指纹记录。请核对后再决定是否信任。");
            }
            ui.separator();
            // 指纹要能选中复制(spec §4.4)。egui 的 `interaction.selectable_labels`
            // 默认为 true,`ui.monospace` 产出的 Label 天然可框选——不要为此改样式,
            // 但也不要把它换成 `ui.small`/自绘文本,那会把可选性弄丢。
            egui::Grid::new("host-key-facts")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("主机");
                    ui.monospace(view.host);
                    ui.end_row();
                    ui.label("算法");
                    ui.monospace(view.algo);
                    ui.end_row();
                    if let Some(prev) = view.previous {
                        ui.label("原记录");
                        ui.monospace(prev);
                        ui.end_row();
                        ui.label("本次收到");
                        ui.colored_label(
                            egui::Color32::from_rgb(200, 40, 40),
                            egui::RichText::new(view.fingerprint).monospace(),
                        );
                        ui.end_row();
                    } else {
                        ui.label("指纹");
                        ui.monospace(view.fingerprint);
                        ui.end_row();
                    }
                });
            ui.separator();
            ui.label("在服务器本机上核对:");
            ui.monospace("ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub");
            let left = view.grace_left_secs();
            if left == 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 40, 40),
                    "已超过握手宽限期,远端可能已断开——取消后重连即可。",
                );
            } else {
                ui.label(format!("远端约 {left} 秒后会因超时断开握手。"));
            }
            ui.separator();
            ui.horizontal(|ui| {
                // 变更态把「取消连接」放在最左(默认位),接受要多走一步。
                if view.is_changed() {
                    if ui.button("取消连接").clicked() {
                        *reply = Some(false);
                    }
                    if ui.button("我已核对,接受并更新记录").clicked() {
                        *reply = Some(true);
                    }
                } else {
                    if ui.button("接受并记住").clicked() {
                        *reply = Some(true);
                    }
                    if ui.button("取消连接").clicked() {
                        *reply = Some(false);
                    }
                }
            });
        });
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p mullion-app ui::host_key 2>&1 | tail -20`
Expected: `test result: ok.`,2 个测试通过。

- [ ] **Step 5: 接进 build_ui 与事件循环**

5a. `crates/mullion-app/src/ui/mod.rs` 的 `UiState`(`:71` 的 `pick_key_request` 之后)加:

```rust
    /// 主机密钥弹窗的回答(F3)。`Some(true)` = 接受;`Some(false)` = 取消连接。
    /// 同样只承载意图:record + save + 回送 oneshot 都在 app.rs 施加点做。
    pub host_key_reply: Option<bool>,
```

5b. `build_ui`(`:77-84`)签名末尾加一个参数,并在函数体最前面画弹窗:

```rust
pub fn build_ui(
    ctx: &egui::Context,
    ui_state: &mut UiState,
    sessions: &[SessionRecord],
    store_available: bool,
    connected: bool,
    status: &str,
    host_key: Option<host_key::HostKeyView<'_>>,
) {
    // 主机密钥确认最先画:它是安全关口,任何时候都该盖在最上层(F3)。
    if let Some(view) = &host_key {
        host_key::show(ctx, view, &mut ui_state.host_key_reply);
    }
    chrome::top_menu(ctx, ui_state, connected);
```

其余函数体不动。

5c. `crates/mullion-app/src/app.rs:442` 的 `modal` 表达式加一项(T8 同源:弹窗开着时键盘必须归 egui,否则用户在确认窗上按不了 Tab/回车):

```rust
        let modal = self.ui.session_manager_open
            || self.ui.about_open
            || self.ui.editor_open
            || self.pending_host_key.is_some();
```

5d. `render_frame` 签名(`:727-735`)末尾加参数,并透传:

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
) -> std::time::Duration {
```

`:741` 那一行改成:

```rust
        crate::ui::build_ui(
            ctx,
            ui_state,
            sessions,
            store_available,
            connected,
            status,
            host_key,
        );
```

5e. 调用点(`:578` 的 `let store_available = ...` 之后)先构造视图,再传进去:

```rust
                            let store_available = self.store.is_some();
                            // 借 self.pending_host_key / self.host_key_since 与下面
                            // `&mut self.ui` 是不相干字段,可同时借出。
                            let host_key_view = self.pending_host_key.as_deref().map(|p| {
                                crate::ui::host_key::HostKeyView {
                                    host: &p.host,
                                    algo: &p.algo,
                                    fingerprint: &p.fingerprint,
                                    previous: p.previous.as_ref().map(|e| e.fingerprint.as_str()),
                                    elapsed_secs: self
                                        .host_key_since
                                        .map_or(0, |t| t.elapsed().as_secs()),
                                }
                            });
                            let repaint_delay = render_frame(
                                a,
                                pane,
                                &mut self.ui,
                                sessions,
                                store_available,
                                connected,
                                &status,
                                host_key_view,
                            );
```

5f. intent 施加点:在 `connect_request` 那一块(`:691-702`)之后、`}` 之前插入:

```rust
                // F3:主机密钥弹窗的回答。record + save 必须在 GUI 线程做——
                // store 是同步 IO,而且失败要能落进 last_error 让用户看见。
                if let Some(accept) = self.ui.host_key_reply.take() {
                    if let Some(prompt) = self.pending_host_key.take() {
                        self.host_key_since = None;
                        if accept {
                            diag::mark(diag::Stage::StoreIo);
                            let mut kh = self.known_hosts.lock().expect("known-hosts poisoned");
                            kh.record(
                                &prompt.host,
                                HostKeyEntry {
                                    algo: prompt.algo.clone(),
                                    fingerprint: prompt.fingerprint.clone(),
                                },
                            );
                            // 落盘失败不阻断本次连接:指纹已在内存表里,连接照常;
                            // 代价只是下次启动会再问一遍。
                            if let Err(e) = kh.save() {
                                self.ui.last_error =
                                    Some(format!("主机指纹未能保存:{e}(本次连接不受影响)"));
                            }
                        }
                        // 送回握手线程。Err = 对端已走(超时/断开),没什么可做的。
                        let _ = prompt.reply.send(accept);
                    }
                }
```

- [ ] **Step 6: 跑绿**

Run: `cargo test --workspace 2>&1 | tail -20 && cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20 && cargo fmt --check`
Expected: 全绿、clippy 无输出、fmt 无输出。

若 clippy 报 `if_same_then_else` 或 `collapsible_if`(Step 3 的按钮分支),按提示改;**不要**为了过 clippy 把两态的按钮顺序合并——顺序差异是这条 UI 的全部意义。

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/ui/host_key.rs crates/mullion-app/src/ui/mod.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 主机密钥确认弹窗 + 接受后落盘 (F3)

未知/变更两态(变更态红字、并列旧指纹、默认按钮是取消)、无关闭按钮、
LoginGraceTime 倒计时。弹窗开着时计入 modal(T8:键盘归 egui)。
守护测试:ui::host_key::tests::changed_state_is_marked_dangerous_and_defaults_to_cancel。"
```

---

### Task 10: 文档 + 交付

**Files:**
- Modify: `docs/gui-render-gotchas.md`
- Modify: `Cargo.toml:12`(版本)
- Create: `/tmp/notes.md`(发版说明,不入库)

- [ ] **Step 1: 补踩坑文档**

在 `docs/gui-render-gotchas.md` 的 egui 段末尾追加:

```markdown
### `EventLoopProxy` 只有 `Send`,没有 `Sync`

**症状**:把 `EventLoopProxy<UserEvent>` 直接放进要做成 `Arc<dyn Trait>` 的结构体,
编译报 `EventLoopProxy<UserEvent> cannot be shared between threads safely`。

**规则**:winit 0.30 的 `platform_impl::EventLoopProxy` 内部是个 `Sender`,只
`unsafe impl<T: Send> Send`,没有 `Sync`。要跨线程共享就包一层
`std::sync::Mutex`(`Mutex<T>: Sync where T: Send`),锁只在 `send_event` 那一瞬
持有,**绝不跨 `.await`**。

**守护**:`host_key::PromptingPolicy`(它必须满足 `HostKeyPolicy: Send + Sync`)。

### 弹窗承载「安全决策」时不要给关闭按钮

**症状**:给主机密钥确认窗加 `.open(&mut open)` 后,用户点 X 关掉,握手线程
永远挂在 `oneshot::Receiver` 上——直到 sshd 的 `LoginGraceTime`(默认 120s)把
连接掐掉,期间 UI 看起来「什么都没发生」。

**规则**:凡是有线程正挂着等回答的弹窗,只能通过明确的动作按钮关闭;若确实要给
关闭按钮,关闭路径必须等价于「拒绝」。

**守护**:`ui::host_key::show` 不带 `.open()`;`PromptingPolicy` 里
sender 被丢弃 → `rx.await` 返回 `Err` → 判 Reject(fail-closed)。
```

- [ ] **Step 2: 升版本并跑绿**

把 `Cargo.toml:12` 的 `version = "0.1.6"` 改成 `version = "0.1.7"`,然后:

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log | tail -20
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Expected: 无 FAILED / panicked,clippy 与 fmt 无输出。

```bash
git add docs/gui-render-gotchas.md Cargo.toml Cargo.lock
git commit -m "chore: 版本 0.1.7(滚动回溯 F17 + TOFU 指纹持久化 F3)"
```

- [ ] **Step 3: 交叉编译并做依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app
cp target/x86_64-pc-windows-gnu/release/mullion-app.exe /tmp/mullion.exe
x86_64-w64-mingw32-objdump -p /tmp/mullion.exe | grep 'DLL Name'
```

Expected: 输出里**不得**出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll`
(出现即不合格,按 `docs/cross-compile-windows.md` 修静态链接后重来)。

- [ ] **Step 4: 发 Release**

```bash
cd /tmp && sha256sum mullion.exe > mullion.exe.sha256 && cat mullion.exe.sha256
```

把下面这段写进 `/tmp/notes-body.md`(Release 标题只能是纯版本号 `v0.1.7`,
摘要一律写进正文):

```markdown
## 修了什么
- F17 滚动回溯:默认 10000 行 scrollback;鼠标滚轮三档分流(本地翻阅 / 上报远端 /
  退化成方向键)、Shift+滚轮强制本地翻阅、Shift+PageUp/PageDown 整屏翻页;
  一按普通键立刻跳回底部。
- F3 TOFU 指纹持久化:指纹存 `%APPDATA%\mullion\known_hosts.toml`(明文,
  `SHA256:` 格式,可用 `ssh-keygen -lf` 核对);首次连接弹窗确认,
  指纹变更走红色警告态并列出新旧指纹,默认按钮是「取消连接」。

## 人工验收清单(无头环境验不了)
1. 远端 `seq 1 5000` 后滚轮向上,能翻到历史;滚动中光标滚出视口后应消失。
2. `htop`(全屏 TUI,有鼠标上报)里滚轮应滚动 htop 自己的列表;按住 Shift 滚轮
   应改为翻阅本地历史(T5 逃生门同源)。
3. 无鼠标上报的 alt screen(如 `less` 未开鼠标)里滚轮应等价于上下方向键。
4. 滚到历史中间时随便按一个键 → 立刻跳回底部并把该键送到远端。
5. 首次连一台新主机 → 弹「主机密钥确认」,指纹与服务器上
   `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` 的输出一致;点接受后
   连上,退出重开再连**不应**再弹。
6. 手工把 `known_hosts.toml` 里某主机的 fingerprint 改一个字符 → 再连该主机
   应弹红色「主机密钥已变更」,点「取消连接」后连接失败且不改存档。
7. 把 `known_hosts.toml` 写成乱码 → 启动不崩、连接时重新弹窗;接受后目录里应
   出现 `known_hosts.toml.bak`,原乱码内容完整保留。
8. 弹窗开着时窗口不卡、倒计时每秒走一格;不点任何按钮等 120s,远端断开后
   点「取消连接」不崩。
```

sha256 段用命令拼进去,避免手抄出错:

```bash
{ cat /tmp/notes-body.md; printf '\n## sha256\n\n```\n'; cat /tmp/mullion.exe.sha256; printf '```\n'; } > /tmp/notes.md
```

```bash
HTTPS_PROXY=http://127.0.0.1:7890 gh release create v0.1.7 \
  /tmp/mullion.exe /tmp/mullion.exe.sha256 -t "v0.1.7" -F /tmp/notes.md \
  --repo kilobitcy/Mullion
```

- [ ] **Step 5: 报给用户**

Release 链接 + sha256 + 上面的人工验收清单。

**注意:分支合回 main 前必扫暂存内容**(脱敏纪律):

```bash
git diff --cached | grep -inE "192\.168\.|10\.0\.|/home/me|PRIVATE KEY|ghp_|github_pat_"
```

无输出才可 `git merge --squash` 合回 main 并推送。
