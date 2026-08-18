# 光标形态 + 输入法内联 + 文件图标 + 断线重连 实现 Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让光标跟随远端 DECSCUSR 并默认竖线闪烁(F125)、中文输入时拼音内联可见(F126)、
SFTP 图标按类型可辨(F127)、SSH 断线后自动退避重连(F128)、断连分屏可用 `Ctrl+D` 关掉(F129)。

**Architecture:** 每一项都先落一个**纯函数**(可在无头容器单测),再接线到渲染/事件循环。
光标形状直接取 `alacritty_terminal::Term::cursor_style()`,不自己解析 VT;闪烁相位是
`now_ms` 的纯函数,靠既有 `next_frame_at`/`WaitUntil` 机制排唤醒(T3/T7);重连先修根因
(russh 默认没有 keepalive → 半开链路根本检测不到),再按 host 分组退避重拨,换挂时
**保留 emulator**(与既有 `rehost_pane` 的唯一差别)。

**Tech Stack:** Rust / alacritty_terminal 0.26 / vte 0.15 / russh 0.54.5 / winit 0.30 /
wgpu 23 / glyphon / egui 0.30。

**设计文档:** `docs/superpowers/specs/2026-08-18-cursor-ime-icons-reconnect-design.md`

---

## 通用纪律(每个任务都适用)

- **测试先写、先看它红**。测试写完必须真的跑一次并看到失败信息符合预期,
  再写实现。没见过红的测试不算测试。
- **守护测试要能自证变红**:每条测试的文档注释里写清「把生产代码的哪一行改成什么,
  这条测试就会变红」。
- **源码级守护测试的锚点必须带行首缩进**(例:`"            self.close_active_tab();"`)。
  不带缩进的话,`include_str!` 出来的源码里会匹配到**测试自己那一行**,测试永远绿
  ——这是本项目已经实证过的第五类恒绿模式。
- 大输出先落盘再 grep:
  `cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log`
- 「绿」= `cargo test --workspace` 全过 **且** `cargo clippy --workspace --all-targets -- -D warnings`
  无输出 **且** `cargo fmt --check` 通过。只跑单个 crate 不叫绿。
- 每个任务结束提交一次,commit 摘要带 spec 编号。

---

# 第一部分:F125 光标形状与闪烁

## Task 1: `mullion-term` 把光标形状/闪烁位接出来

**Files:**
- Modify: `crates/mullion-term/src/snapshot.rs`(`Cursor` 结构)
- Modify: `crates/mullion-term/src/emulator.rs`(`with_history` 的 `Config`、`snapshot()`、`cursor()`)

**背景:** `alacritty_terminal 0.26` 已经解析了 DECSCUSR(`term/mod.rs:2204 set_cursor_style`),
`Term::cursor_style()` 返回 `vte::ansi::CursorStyle { shape, blinking }`。我们只是把它接出来,
**不写任何 VT 解析代码**。`vte::ansi::CursorShape` 有 5 个变体:
`Block / Underline / Beam / HollowBlock / Hidden`。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-term/src/emulator.rs` 的 `mod tests` 里:

```rust
    /// F125:远端一言不发时,光标必须是**竖线 + 闪烁** —— 这是用户要的默认。
    ///
    /// 自证会变红:把 `with_history` 里 `default_cursor_style` 那两行删掉
    /// (回到 alacritty 的默认 `Block` + 不闪)。
    #[test]
    fn default_cursor_is_a_blinking_beam() {
        let emu = Emulator::new(20, 5);
        let c = emu.snapshot().cursor;
        assert_eq!(c.shape, CursorShape::Beam, "默认该是竖线");
        assert!(c.blinking, "默认该闪");
    }

    /// F125:远端用 DECSCUSR(`CSI Ps SP q`)要什么形状就给什么形状。
    /// Ps: 0/1=闪块 2=稳定块 3=闪下划线 4=稳定下划线 5=闪竖线 6=稳定竖线。
    ///
    /// 自证会变红:把 `snapshot()` 里 `shape:` 那一行改成写死
    /// `CursorShape::Beam`。
    #[test]
    fn decscusr_selects_shape_and_blink() {
        for (ps, want_shape, want_blink) in [
            (b"1", CursorShape::Block, true),
            (b"2", CursorShape::Block, false),
            (b"3", CursorShape::Underline, true),
            (b"4", CursorShape::Underline, false),
            (b"5", CursorShape::Beam, true),
            (b"6", CursorShape::Beam, false),
        ] {
            let mut emu = Emulator::new(20, 5);
            let mut seq = b"\x1b[".to_vec();
            seq.extend_from_slice(ps);
            seq.extend_from_slice(b" q");
            emu.feed(&seq);
            let c = emu.snapshot().cursor;
            assert_eq!(c.shape, want_shape, "Ps={} 的形状", ps[0] as char);
            assert_eq!(c.blinking, want_blink, "Ps={} 的闪烁位", ps[0] as char);
        }
    }

    /// `cursor()` 是 `snapshot().cursor` 的轻量同源版,新加的两个字段同样必须同源
    /// ——只在 `snapshot()` 里填、`cursor()` 里漏掉的话,IME 定位那条路径拿到的
    /// 形状恒是默认值。
    ///
    /// 自证会变红:把 `cursor()` 里的 `shape` 改成写死 `CursorShape::Block`。
    #[test]
    fn lightweight_cursor_agrees_on_shape_and_blink() {
        let mut emu = Emulator::new(20, 5);
        emu.feed(b"\x1b[4 q");
        assert_eq!(emu.cursor(), emu.snapshot().cursor);
    }
```

在该 `mod tests` 顶部的 `use super::*;` 之外补一行:

```rust
    use crate::snapshot::CursorShape;
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-term default_cursor_is_a_blinking_beam 2>&1 | tail -20
```

预期:编译失败,`no variant or associated item named 'Beam'` / `struct 'Cursor' has no field named 'shape'`。

- [ ] **Step 3: `snapshot.rs` 加类型**

把 `Cursor` 改成:

```rust
/// 光标形状(F125)。**本 crate 自己的枚举**,不把 `vte::ansi::CursorShape`
/// 漏进公开 API —— 架构不变量要求 `mullion-app` 只认识 `mullion-term` 的类型,
/// 而且 alacritty 将来加变体时,映射处会编译报错而不是被 `_ =>` 悄悄吞掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    /// 实心块。
    Block,
    /// 下划线。
    Underline,
    /// 竖线。**本项目的默认**(见 `Emulator::with_history`)。
    #[default]
    Beam,
    /// 空心框。远端主动要求时才会出现(我们自己用它表示"非焦点 pane",
    /// 那条路径不看这个字段)。
    HollowBlock,
    /// 远端要求不画光标。
    Hidden,
}

/// 光标快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    /// F125:远端 DECSCUSR 要求的形状,没要求过就是 `Beam`(本项目默认)。
    pub shape: CursorShape,
    /// F125:远端要求闪不闪,没要求过就是 `true`。
    pub blinking: bool,
}
```

- [ ] **Step 4: `emulator.rs` 设默认 + 填字段**

`with_history` 里的 `Config` 改成:

```rust
        let config = Config {
            scrolling_history: history,
            // F125:远端没发 DECSCUSR 时的形状。项目默认是**闪烁竖线**,
            // 不是 alacritty 的实心块 —— 这是用户明确要的默认。
            default_cursor_style: alacritty_terminal::vte::ansi::CursorStyle {
                shape: alacritty_terminal::vte::ansi::CursorShape::Beam,
                blinking: true,
            },
            ..Config::default()
        };
```

在文件里加映射函数(放在 `Emulator` impl 之外、`GridSize` 附近):

```rust
/// `vte` 的形状 → 本 crate 的形状。**穷尽 match,不留 `_` 兜底**:
/// alacritty 加新变体时这里编译报错,比悄悄退化成某个形状好。
fn map_shape(s: alacritty_terminal::vte::ansi::CursorShape) -> CursorShape {
    use alacritty_terminal::vte::ansi::CursorShape as V;
    match s {
        V::Block => CursorShape::Block,
        V::Underline => CursorShape::Underline,
        V::Beam => CursorShape::Beam,
        V::HollowBlock => CursorShape::HollowBlock,
        V::Hidden => CursorShape::Hidden,
    }
}
```

`snapshot()` 里构造 `Cursor` 的地方(约 `emulator.rs:264`)与 `cursor()` 里那处
(约 `emulator.rs:285`)**各加两行**,两处都从同一个来源取:

```rust
            let style = self.term.cursor_style();
            // ...
            cursor: Cursor {
                row: cursor_row.max(0) as u16,
                col: p.column.0 as u16,
                visible: cursor_row >= 0 && (cursor_row as usize) < rows,
                shape: map_shape(style.shape),
                blinking: style.blinking,
            },
```

文件顶部的 `use crate::snapshot::{...}` 补上 `CursorShape`。

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test -p mullion-term 2>&1 | tail -20
```

预期:全过(其它构造 `Cursor` 的地方若报缺字段,补上 `..Default::default()` 不行——
`Cursor` 没有 `Default`,老实把两个字段写出来)。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-term/src/snapshot.rs crates/mullion-term/src/emulator.rs
git commit -m "feat(term): 光标形状/闪烁位接出 DECSCUSR,默认闪烁竖线 (F125)"
```

---

## Task 2: 闪烁相位纯函数

**Files:**
- Modify: `crates/mullion-app/src/frame.rs`

**背景:** 闪烁 = 周期重绘,是 T3(每秒几千次重绘)/T7(节流后忙转)的雷区。相位必须是
**纯函数**,才能脱离 GPU 单测,且让「什么时候需要下一次唤醒」有唯一定义源。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/frame.rs` 的 `mod tests`:

```rust
    /// F125:半周期 530ms(Windows 系统默认光标闪烁周期)。
    /// 相位从**最后一次输入**起算,不是从进程启动起算。
    #[test]
    fn blink_alternates_every_half_period() {
        assert!(blink_visible(0, 0), "刚输入完必须是亮的");
        assert!(blink_visible(529, 0), "半周期内一直亮");
        assert!(!blink_visible(530, 0), "过半周期转灭");
        assert!(!blink_visible(1059, 0));
        assert!(blink_visible(1060, 0), "一个整周期后回到亮");
    }

    /// F125:打字必须重置相位 —— 不重置的话连续打字时光标会随机隐没,
    /// 观感像丢帧。判据:同一时刻,`last_input` 变了结果就该跟着变。
    ///
    /// 自证会变红:把 `blink_visible` 里的 `now_ms - last_input_ms` 换成 `now_ms`。
    #[test]
    fn typing_resets_the_phase() {
        assert!(!blink_visible(600, 0), "距上次输入 600ms:灭");
        assert!(blink_visible(600, 600), "这一刻刚敲了键:立刻回到亮");
    }

    /// 时钟倒退(不同来源的 now)不能 panic —— 用饱和减法。
    #[test]
    fn clock_going_backwards_is_not_a_panic() {
        assert!(blink_visible(0, 999));
    }

    /// F125:下一次相位翻转的时刻,给事件循环排 `WaitUntil` 用。
    /// **不返回固定的 530**:刚翻转过的那一帧要等满 530,翻转前 10ms 就只等 10ms,
    /// 否则光标会晚一整拍。
    ///
    /// 自证会变红:把 `blink_next_flip_ms` 的函数体改成 `BLINK_HALF_MS`。
    #[test]
    fn next_flip_is_the_remainder_of_the_current_half_period() {
        assert_eq!(blink_next_flip_ms(0, 0), 530);
        assert_eq!(blink_next_flip_ms(520, 0), 10);
        assert_eq!(blink_next_flip_ms(530, 0), 530);
        assert_eq!(blink_next_flip_ms(1000, 0), 60);
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib frame:: 2>&1 | tail -20
```

预期:`cannot find function 'blink_visible' in this scope`。

- [ ] **Step 3: 实现**

追加到 `crates/mullion-app/src/frame.rs`(放在 `frame_is_dirty` 后面):

```rust
/// 光标闪烁半周期(毫秒)。530ms 是 Windows 的系统默认光标闪烁周期,
/// 与用户在别处(记事本、cmd)看到的节奏一致。
pub const BLINK_HALF_MS: u64 = 530;

/// F125:这一刻光标该不该画出来。
///
/// 相位从**最后一次键盘输入**起算:刚敲完一个字符,光标一定是亮的。不这么做的话
/// 连续打字时光标会随机隐没,观感像丢帧。
///
/// 纯函数,不碰时钟也不碰 winit —— 闪烁是本项目里唯一一个「必须周期性重绘」的
/// 特性,把判据留在能单测的地方,是它不退化成 T3(每帧重绘)的前提。
pub fn blink_visible(now_ms: u64, last_input_ms: u64) -> bool {
    let elapsed = now_ms.saturating_sub(last_input_ms);
    (elapsed / BLINK_HALF_MS) % 2 == 0
}

/// F125:距下一次相位翻转还有多少毫秒。调用方据此排一次 `WaitUntil`
/// (**不是** `request_redraw`:那会绕开帧闸,踩 T3/T7)。
pub fn blink_next_flip_ms(now_ms: u64, last_input_ms: u64) -> u64 {
    let elapsed = now_ms.saturating_sub(last_input_ms);
    BLINK_HALF_MS - (elapsed % BLINK_HALF_MS)
}
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib frame:: 2>&1 | tail -10
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/frame.rs
git commit -m "feat(app): 光标闪烁相位纯函数(530ms 半周期,打字重置) (F125)"
```

---

## Task 3: `gpu.rs` 按形状画光标

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs`

**背景:** 现在 `CursorStyle` 只有 `Block`/`Hollow` 两个变体、且写死「焦点=实心、非焦点=空心」。
改成:**焦点 pane 按快照里的形状画,非焦点 pane 恒空心框且恒不闪**。后半条优先级高于
「忠实呈现远端形状」—— 4 块分屏一起闪会让人看不出焦点在哪。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/gpu.rs` 的 `mod tests`:

```rust
    /// F125:竖线光标只占 `BAR_PX` 宽,不是整格 —— 画成整格就是原来的实心块。
    ///
    /// 自证会变红:把 `CursorStyle::Bar` 那一支的 `w: BAR_PX` 改成 `w: cell_w`。
    #[test]
    fn beam_cursor_is_a_thin_bar_at_the_cell_left_edge() {
        let snap = snap_with_cursor(2, 1, CursorShape::Beam);
        let q = cursor_quads(&snap, CursorStyle::Bar);
        assert_eq!(q.len(), 1, "竖线是一个 quad");
        assert_eq!(q[0].w, BAR_PX, "宽度是 BAR_PX");
        assert_eq!(q[0].h, 20.0, "高度占满整格");
        assert_eq!(q[0].x, 2.0 * 10.0, "贴在格子左缘");
    }

    /// F125:下划线光标贴格子底缘,高 `BAR_PX`。
    ///
    /// 自证会变红:把 `Underline` 那一支的 `y` 改成 `y`(不加 `cell_h - BAR_PX`)。
    #[test]
    fn underline_cursor_sits_on_the_cell_bottom() {
        let snap = snap_with_cursor(0, 0, CursorShape::Underline);
        let q = cursor_quads(&snap, CursorStyle::Underline);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].h, BAR_PX);
        assert_eq!(q[0].y, 20.0 - BAR_PX, "贴底");
    }

    /// F125:远端要求隐藏光标(`CSI 0 SP q` 之外还有 `DECTCEM`)时一个 quad 都不画。
    ///
    /// 自证会变红:把 `CursorStyle::None` 那一支改成 `Block` 的画法。
    #[test]
    fn hidden_cursor_draws_nothing() {
        let snap = snap_with_cursor(0, 0, CursorShape::Hidden);
        assert!(cursor_quads(&snap, CursorStyle::None).is_empty());
    }

    /// F125:**非焦点 pane 恒空心框、且不看快照里的形状**。
    /// 4 块分屏一起闪 / 一起画竖线的话,用户看不出键盘输入进了哪一块(§7.1)。
    ///
    /// 自证会变红:把 `style_for` 里非焦点那一支改成跟焦点一样走 `from_shape`。
    #[test]
    fn unfocused_pane_is_always_hollow_regardless_of_remote_shape() {
        for shape in [
            CursorShape::Beam,
            CursorShape::Block,
            CursorShape::Underline,
            CursorShape::Hidden,
        ] {
            assert_eq!(
                style_for(shape, false, true),
                CursorStyle::Hollow,
                "{shape:?} 在非焦点 pane 上必须是空心框"
            );
        }
    }

    /// F125:焦点 pane 上,闪烁到「灭」的那半周期不画光标;非焦点 pane 不受影响
    /// (它本来就不闪,`blink_on` 传什么都是空心框)。
    ///
    /// 自证会变红:把 `style_for` 里 `if !blink_on` 那一支删掉。
    #[test]
    fn blink_off_hides_only_the_focused_cursor() {
        assert_eq!(style_for(CursorShape::Beam, true, false), CursorStyle::None);
        assert_eq!(
            style_for(CursorShape::Beam, false, false),
            CursorStyle::Hollow
        );
    }
```

同一个 `mod tests` 里加两个脚手架(放在测试之前):

```rust
    use mullion_term::snapshot::CursorShape;

    /// 造一个 cols=4 rows=2、光标在 (row, col) 且形状为 `shape` 的快照。
    fn snap_with_cursor(col: u16, row: u16, shape: CursorShape) -> GridSnapshot {
        let blank = SnapCell {
            ch: ' ',
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
            width: 1,
            spacer: false,
            selected: false,
        };
        GridSnapshot {
            cols: 4,
            rows: 2,
            cells: vec![blank; 8],
            cursor: mullion_term::snapshot::Cursor {
                row,
                col,
                visible: true,
                shape,
                blinking: true,
            },
        }
    }

    /// 只取光标那部分 quad:格子底色全是默认色,`quads_for` 不会为它们出 quad,
    /// 所以剩下的就是光标。
    ///
    /// **注意**:Task 7 会给 `quads_for` 加一个 `cursor_col_override` 参数,
    /// 到时候这个脚手架里补一个 `None` 即可(只改这一处,所有用它的测试跟着走)。
    fn cursor_quads(snap: &GridSnapshot, style: CursorStyle) -> Vec<Quad> {
        quads_for(
            snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors {
                fg: Rgb::new(0xcc, 0xcc, 0xcc),
                bg: Rgb::new(0x10, 0x10, 0x10),
            },
            style,
        )
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib gpu:: 2>&1 | tail -20
```

预期:`no variant named 'Bar'` / `cannot find function 'style_for'`。

- [ ] **Step 3: 实现**

`gpu.rs` 里把 `CursorStyle` 换成:

```rust
/// 光标画法(F125)。多 pane 下必须区分:4 个 pane 同时亮 4 个光标的话,
/// 用户看不出键盘输入进了哪一块(§7.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    /// 实心块(远端 DECSCUSR 要 Block 时)。
    Block,
    /// 竖线,本项目默认。
    Bar,
    /// 下划线。
    Underline,
    /// 空心框:**非焦点 pane 恒用这个**,远端要 HollowBlock 时也是它。
    Hollow,
    /// 不画:远端要求隐藏,或焦点 pane 闪到了「灭」的半周期。
    None,
}

/// 竖线宽 / 下划线高(像素)。1px 在高 DPI 下几乎看不见,和 `HOLLOW_PX` 一样
/// 不做 DPI 缩放(同一档处理)。
pub const BAR_PX: f32 = 2.0;

/// 这一帧该怎么画这个 pane 的光标(F125)。**唯一判据源**——`quads_for_panes`
/// 与测试都走它,不许各写一份。
///
/// - 非焦点 pane:恒 `Hollow`,不看远端形状、不看闪烁相位。
/// - 焦点 pane:远端要什么形状给什么;闪到「灭」的半周期就不画。
pub fn style_for(shape: mullion_term::snapshot::CursorShape, focused: bool, blink_on: bool) -> CursorStyle {
    use mullion_term::snapshot::CursorShape as S;
    if !focused {
        return CursorStyle::Hollow;
    }
    if !blink_on {
        return CursorStyle::None;
    }
    match shape {
        S::Block => CursorStyle::Block,
        S::Beam => CursorStyle::Bar,
        S::Underline => CursorStyle::Underline,
        S::HollowBlock => CursorStyle::Hollow,
        S::Hidden => CursorStyle::None,
    }
}
```

`quads_for` 里 `match cursor` 那段扩成五支(`Block`/`Hollow` 两支保持原样,新增三支):

```rust
            CursorStyle::Bar => quads.push(Quad {
                x,
                y,
                w: BAR_PX,
                h: cell_h,
                color,
            }),
            CursorStyle::Underline => quads.push(Quad {
                x,
                y: y + cell_h - BAR_PX,
                w: cell_w,
                h: BAR_PX,
                color,
            }),
            CursorStyle::None => {}
```

`quads_for_panes` 的签名加一个 `blink_on: bool`,里面那段

```rust
        let style = if p.focused {
            CursorStyle::Block
        } else {
            CursorStyle::Hollow
        };
```

换成:

```rust
        let style = style_for(p.snap.cursor.shape, p.focused, blink_on);
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib gpu:: 2>&1 | tail -10
```

调用方(`app.rs` 里 `quads_for_panes(...)` 那一处)先临时传 `true` 让它编过,
Task 4 再接真值。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/gpu.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 光标按 DECSCUSR 形状画,非焦点恒空心框 (F125)"
```

---

## Task 4: 闪烁接进事件循环(T3/T7 红线)

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

**背景:** `app.rs` 已有一套「排下一次唤醒」的机制:`self.next_frame_at` + 三处
`event_loop.set_control_flow(ControlFlow::WaitUntil(at))`,其中 T2 的同步块超时走
`sync_timeout_wake(now)`。闪烁**并进这条链路**,不新开定时路径,也绝不调
`request_redraw`(那会绕开帧闸,踩 T3/T7)。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/app.rs` 的 `mod tests`:

```rust
    /// **接线守护 / F125**:闪烁的下一次唤醒必须并进既有的「定时唤醒」判据,
    /// 而不是各自为政 —— 两个定时源各排各的,后写的那个会把先写的覆盖掉,
    /// 症状是「光标闪起来之后同步块超时不再收口」(T2 复发)。
    ///
    /// 自证会变红:把 `next_timer_wake` 的函数体改成只 `self.sync_timeout_wake(now)`。
    #[test]
    fn timer_wakeups_are_merged_in_one_place() {
        let src = include_str!("app.rs");
        let after = src
            .split("    fn next_timer_wake(")
            .nth(1)
            .expect("找不到 next_timer_wake 的定义");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        assert!(
            body.contains("self.sync_timeout_wake(now)"),
            "定时唤醒没并进同步块超时 —— T2 会复发"
        );
        assert!(
            body.contains("self.blink_wake("),
            "定时唤醒没并进光标闪烁 —— 光标只会在别的事件顺带唤醒时才翻转"
        );
    }

    /// **接线守护 / F125**:闪烁只许排 `WaitUntil`,不许 `request_redraw`。
    /// 后者绕开帧闸,是 T3(GPU 空转)/T7(100% CPU 忙转)的直接触发方式。
    ///
    /// 自证会变红:在 `blink_wake` 里加一句 `self.request_ui_redraw();`。
    #[test]
    fn blink_never_forces_a_redraw() {
        let src = include_str!("app.rs");
        let after = src
            .split("    fn blink_wake(")
            .nth(1)
            .expect("找不到 blink_wake 的定义");
        let body = &after[..after.find("\n    }\n").expect("找不到函数结尾")];
        assert!(
            !body.contains("request_redraw"),
            "闪烁不许直接请求重绘,只能排 WaitUntil(T3/T7)"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib timer_wakeups_are_merged_in_one_place 2>&1 | tail -10
```

预期:`panicked at '找不到 next_timer_wake 的定义'`。

- [ ] **Step 3: 实现**

`App` 结构加两个字段(挨着既有的 `next_frame_at` 写):

```rust
    /// F125:最后一次**键盘输入**的时刻,光标闪烁相位从它起算(打字重置相位)。
    /// 用 `Instant` 而不是 `u64`:与 `next_frame_at`/`sync_timeout_wake` 同一套时钟。
    last_input_at: Instant,
    /// F125:窗口有没有焦点。失焦时不闪(也就不需要周期唤醒),与 Windows 上
    /// 其它终端的惯例一致。
    window_focused: bool,
```

构造 `App` 的地方给初值:`last_input_at: Instant::now()`、`window_focused: true`。

加三个方法(放在 `sync_timeout_wake` 旁边):

```rust
    /// F125:这一帧光标该不该画出来。失焦时恒 `true`(不闪,常显)——
    /// 焦点 pane 在失焦状态下由 `style_for` 收到 `focused=false`,画成空心框。
    fn blink_on(&self, now: Instant) -> bool {
        if !self.window_focused {
            return true;
        }
        let elapsed = now.saturating_duration_since(self.last_input_at).as_millis() as u64;
        crate::frame::blink_visible(elapsed, 0)
    }

    /// F125:下一次光标相位翻转的时刻。`None` = 这一刻不需要为闪烁排唤醒
    /// (窗口失焦 / 没有终端在前台)。
    fn blink_wake(&self, now: Instant) -> Option<Instant> {
        if !self.window_focused || self.active_ws().is_none() {
            return None;
        }
        let elapsed = now.saturating_duration_since(self.last_input_at).as_millis() as u64;
        let ms = crate::frame::blink_next_flip_ms(elapsed, 0);
        Some(now + std::time::Duration::from_millis(ms))
    }

    /// 所有「定时唤醒源」的汇合点:取最早的那个。**新增定时源一律加在这里**,
    /// 各自排各自的 `WaitUntil` 会互相覆盖(后写的赢),症状是某个定时行为
    /// 时灵时不灵。
    fn next_timer_wake(&self, now: Instant) -> Option<Instant> {
        [self.sync_timeout_wake(now), self.blink_wake(now)]
            .into_iter()
            .flatten()
            .min()
    }
```

把 `app.rs` 里**两处** `if let Some(at) = self.sync_timeout_wake(now)`(约 6614 行、
6635 行)改成 `if let Some(at) = self.next_timer_wake(now)`。

键盘输入路径里重置相位 —— `WindowEvent::KeyboardInput` 分支开头(`if event.state == ElementState::Pressed {` 之后)加一行:

```rust
                    // F125:打字重置闪烁相位,保证「刚敲完光标一定是亮的」。
                    self.last_input_at = Instant::now();
```

`WindowEvent::Ime` 的 `Commit` 分支里同样加一行(输入法提交也是输入)。

窗口焦点事件 —— 在 `window_event` 的 match 里加(若已有 `Focused` 分支就在里面加赋值):

```rust
            WindowEvent::Focused(f) => {
                // F125:失焦不闪,省掉后台窗口的周期唤醒。
                self.window_focused = f;
                self.request_ui_redraw();
            }
```

把 Task 3 临时传的 `true` 换成真值 —— 找到 `quads_for_panes(` 的调用处:

```rust
        let blink_on = self.blink_on(Instant::now());
        let quads = crate::gpu::quads_for_panes(&renders, cell_w, cell_h, defaults, blink_on);
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 | tail -20
cargo test -p mullion-app --lib redraw_is_frame_capped 2>&1 | tail -5
cargo test -p mullion-app --lib frame:: 2>&1 | tail -5
```

预期:全过。**T3(`redraw_is_frame_capped`)与 T7(`frame::tests`)必须仍绿** —— 它们是
这一步最容易破坏的东西。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs
git commit -m "feat(app): 光标闪烁并入定时唤醒链路,失焦不闪 (F125)

守护测试:timer_wakeups_are_merged_in_one_place / blink_never_forces_a_redraw;
回归跑了 redraw_is_frame_capped(T3)与 frame::tests(T7)。"
```

---

# 第二部分:F126 输入法拼音内联

## Task 5: `ImeState` 保存 preedit 文本

**Files:**
- Modify: `crates/mullion-app/src/input.rs`

**背景:** 现在 `ImeState` 只有一个 `preediting: bool`,`Ime::Preedit(text, _)` 里的文本被丢掉,
所以屏幕上看不到拼音。三条结束边(`Commit` / 空 `Preedit` / `Disabled`)**都必须清空文本**
——漏一条就会在屏幕上留一串永不消失的幽灵拼音。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/input.rs` 的 `mod tests`:

```rust
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
    /// 任意一句。
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
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib input:: 2>&1 | tail -10
```

预期:`no method named 'preedit'`。

- [ ] **Step 3: 实现**

`ImeState` 改成(注意 `Copy` 要去掉 —— 有 `String` 字段了;检查调用处有没有依赖 `Copy`,
`app.rs` 里是 `self.ime.xxx()` 的方法调用,不受影响):

```rust
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImeState {
    preediting: bool,
    /// F126:组字中的拼音串,要画在光标处。三条结束边都必须清空它。
    text: String,
}

impl ImeState {
    /// 收到 `Ime::Preedit`。空串 = 候选被取消,组字结束。
    pub fn on_preedit(&mut self, text: &str) {
        self.preediting = !text.is_empty();
        self.text.clear();
        self.text.push_str(text);
    }

    /// 收到 `Ime::Commit`,组字结束。
    pub fn on_commit(&mut self) {
        self.preediting = false;
        self.text.clear();
    }

    /// 收到 `Ime::Disabled`(切走输入法 / 失焦),组字结束。
    pub fn on_disabled(&mut self) {
        self.preediting = false;
        self.text.clear();
    }

    /// 这一刻的按键该不该被吞掉(组字中 = 该吞)。
    pub fn swallows_key(&self) -> bool {
        self.preediting
    }

    /// F126:组字中的拼音串,空串 = 没在组字。
    pub fn preedit(&self) -> &str {
        &self.text
    }
}
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib input:: 2>&1 | tail -10
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/input.rs
git commit -m "feat(app): ImeState 保留 preedit 文本,三条结束边都清空 (F126)"
```

---

## Task 6: preedit 布局纯函数

**Files:**
- Modify: `crates/mullion-app/src/text.rs`

**背景:** preedit 要按**终端网格**摆放(CJK 占两格),超出行尾截断,光标停在串末尾。
这套算式必须能脱离 GPU 单测。宽度判定复用项目里已有的那一套(`mullion_term` 用
`unicode-width` 算 `SnapCell::width`),这里同样用它,不另起一套。

- [ ] **Step 1: 确认宽度工具可用**

```bash
grep -rn "unicode-width\|unicode_width" crates/mullion-term/Cargo.toml crates/mullion-app/Cargo.toml crates/mullion-term/src/*.rs | head
```

若 `mullion-app` 的 `Cargo.toml` 里没有 `unicode-width`,加上(版本对齐 `mullion-term` 里那个)。

- [ ] **Step 2: 写失败的测试**

追加到 `crates/mullion-app/src/text.rs` 的 `mod tests`:

```rust
    /// F126:拼音串从光标格开始逐格摆,ASCII 一格一个。
    #[test]
    fn preedit_starts_at_the_cursor_cell() {
        let cells = preedit_layout(20, 3, "abc");
        assert_eq!(cells.len(), 3);
        assert_eq!((cells[0].col, cells[0].ch, cells[0].width), (3, 'a', 1));
        assert_eq!((cells[2].col, cells[2].ch, cells[2].width), (5, 'c', 1));
    }

    /// F126:已转换出的汉字占两格 —— 按一格摆的话,后面的字会左移,
    /// 而底色/下划线是按格子画的,两套定位当场分家。
    ///
    /// 自证会变红:把 `preedit_layout` 里的宽度改成恒 1。
    #[test]
    fn wide_chars_take_two_cells() {
        let cells = preedit_layout(20, 0, "你a");
        assert_eq!((cells[0].col, cells[0].width), (0, 2));
        assert_eq!((cells[1].col, cells[1].width), (2, 1), "汉字之后让开两格");
    }

    /// F126:超出行尾直接截断,不折行 —— preedit 是纯覆盖层,不该有改行内容
    /// 布局的权力。
    ///
    /// 自证会变红:把截断判据 `col + w > cols` 改成 `col > cols`。
    #[test]
    fn preedit_is_truncated_at_the_line_end() {
        let cells = preedit_layout(5, 3, "abcde");
        assert_eq!(cells.len(), 2, "只放得下两格");
        assert_eq!(cells.last().unwrap().col, 4);
    }

    /// F126:宽字符跨不过行尾时整个丢掉,不能只画左半 —— 半个汉字是花屏。
    #[test]
    fn a_wide_char_that_does_not_fit_is_dropped_whole() {
        let cells = preedit_layout(5, 4, "你");
        assert!(cells.is_empty(), "第 4 列放不下两格宽的字");
    }

    /// F126:光标停在拼音串**末尾**(已拍板)。串放不下时停在最后画出来的那格之后。
    ///
    /// 自证会变红:把 `preedit_cursor_col` 改成直接返回 `cursor_col`。
    #[test]
    fn cursor_sits_at_the_end_of_the_preedit() {
        assert_eq!(preedit_cursor_col(20, 3, "abc"), 6);
        assert_eq!(preedit_cursor_col(20, 0, "你a"), 3);
        assert_eq!(preedit_cursor_col(5, 3, "abcde"), 5, "截断后停在行尾");
        assert_eq!(preedit_cursor_col(20, 7, ""), 7, "没在组字就是原位");
    }
```

- [ ] **Step 3: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib text::tests::preedit 2>&1 | tail -10
```

预期:`cannot find function 'preedit_layout'`。

- [ ] **Step 4: 实现**

追加到 `crates/mullion-app/src/text.rs`(放在 `row_to_runs` 后面):

```rust
/// F126:preedit 串里的一格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreeditCell {
    /// 0-based 列号(终端网格)。
    pub col: u16,
    pub ch: char,
    /// 显示宽度:CJK = 2,其余 = 1。
    pub width: u8,
}

/// F126:把组字中的拼音串摆到终端网格上,从光标格起。
///
/// - 宽字符占两格,与 `SnapCell::width` 同一套判据(`unicode-width`),
///   不另起一套 —— 底色/下划线是按格子画的,两套宽度判据会当场错位。
/// - **超出行尾直接截断**,不折行:preedit 是纯覆盖层,不该有改动行内容布局的权力。
/// - 宽字符跨不过行尾时整个丢掉,不画左半(半个汉字是花屏)。
pub fn preedit_layout(cols: u16, cursor_col: u16, text: &str) -> Vec<PreeditCell> {
    use unicode_width::UnicodeWidthChar;
    let mut out = Vec::new();
    let mut col = cursor_col;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0).clamp(1, 2) as u16;
        if col + w > cols {
            break;
        }
        out.push(PreeditCell {
            col,
            ch,
            width: w as u8,
        });
        col += w;
    }
    out
}

/// F126:组字期间光标该画在哪一列 —— 拼音串**末尾**(已拍板)。
/// 空串(没在组字)时就是原位。
pub fn preedit_cursor_col(cols: u16, cursor_col: u16, text: &str) -> u16 {
    match preedit_layout(cols, cursor_col, text).last() {
        Some(c) => c.col + u16::from(c.width),
        None => cursor_col,
    }
}
```

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib text:: 2>&1 | tail -10
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/text.rs crates/mullion-app/Cargo.toml
git commit -m "feat(app): preedit 网格布局纯函数(宽字两格/行尾截断/光标居末) (F126)"
```

---

## Task 7: preedit 画到屏幕上

**Files:**
- Modify: `crates/mullion-app/src/gpu.rs`(底色 + 下划线 quad)
- Modify: `crates/mullion-app/src/text.rs`(preedit 文字进 glyphon)
- Modify: `crates/mullion-app/src/app.rs`(把 `ime.preedit()` 传下去 + IME 区域跟末尾)

**背景:** 画三层,顺序不能错:① 默认背景色的 quad 盖住底下原字符 → ② preedit 文字 →
③ 1px 下划线 quad(glyphon 不画下划线,必须自己画)。光标画在串末尾。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/gpu.rs` 的 `mod tests`:

```rust
    /// F126:preedit 每一格都要先铺一层背景色盖住底下的原字符,再画下划线。
    /// 不铺底的话,拼音会和底下的命令行文字叠在一起,糊成一团。
    ///
    /// 自证会变红:把 `preedit_quads` 里那句 push 背景 quad 删掉。
    #[test]
    fn preedit_covers_the_cells_underneath_and_underlines_them() {
        let cells = vec![
            crate::text::PreeditCell {
                col: 2,
                ch: 'n',
                width: 1,
            },
            crate::text::PreeditCell {
                col: 3,
                ch: 'i',
                width: 1,
            },
        ];
        let d = DefaultColors {
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
        };
        let q = preedit_quads(&cells, (0.0, 0.0), 1, 10.0, 20.0, d);
        // 每格 2 个 quad:底 + 下划线。
        assert_eq!(q.len(), 4);
        let bg: Vec<_> = q.iter().filter(|q| q.h == 20.0).collect();
        assert_eq!(bg.len(), 2, "两格底色");
        assert_eq!(bg[0].x, 20.0);
        assert_eq!(bg[0].y, 20.0, "第 1 行");
        assert_eq!(bg[0].color, [0x10, 0x10, 0x10], "底色 = 默认背景色");
        let ul: Vec<_> = q.iter().filter(|q| q.h == UNDERLINE_PX).collect();
        assert_eq!(ul.len(), 2, "两格下划线");
        assert_eq!(ul[0].y, 20.0 + 20.0 - UNDERLINE_PX, "贴格子底缘");
        assert_eq!(ul[0].color, [0xcc, 0xcc, 0xcc], "下划线 = 默认前景色");
    }

    /// F126:宽字符的底色/下划线要盖满两格,画一格的话汉字右半露出底下的旧字。
    #[test]
    fn wide_preedit_cell_covers_two_columns() {
        let cells = vec![crate::text::PreeditCell {
            col: 0,
            ch: '你',
            width: 2,
        }];
        let d = DefaultColors {
            fg: Rgb::new(0xcc, 0xcc, 0xcc),
            bg: Rgb::new(0x10, 0x10, 0x10),
        };
        let q = preedit_quads(&cells, (0.0, 0.0), 0, 10.0, 20.0, d);
        assert!(q.iter().all(|q| q.w == 20.0), "两格宽");
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib gpu::tests::preedit 2>&1 | tail -10
```

- [ ] **Step 3: 实现 quad 层**

追加到 `crates/mullion-app/src/gpu.rs`:

```rust
/// preedit 下划线粗细(像素)。比光标那条 `BAR_PX` 细:它是"未提交"的标记,
/// 不该比光标本身还抢眼。
pub const UNDERLINE_PX: f32 = 1.0;

/// F126:组字中的拼音串要画的色块 —— 每格一个背景(盖住底下的原字符)+
/// 一条下划线(未提交的标记;glyphon 不画下划线,只能自己来)。
///
/// `origin` 与 `quads_for` 同一个约定:终端区左上角的窗口像素坐标。
/// `row` 是光标所在行(preedit 不跨行)。
pub fn preedit_quads(
    cells: &[crate::text::PreeditCell],
    origin: (f32, f32),
    row: u16,
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
) -> Vec<Quad> {
    let mut out = Vec::with_capacity(cells.len() * 2);
    let y = origin.1 + f32::from(row) * cell_h;
    for c in cells {
        let x = origin.0 + f32::from(c.col) * cell_w;
        let w = f32::from(c.width) * cell_w;
        out.push(Quad {
            x,
            y,
            w,
            h: cell_h,
            color: [defaults.bg.r, defaults.bg.g, defaults.bg.b],
        });
        out.push(Quad {
            x,
            y: y + cell_h - UNDERLINE_PX,
            w,
            h: UNDERLINE_PX,
            color: [defaults.fg.r, defaults.fg.g, defaults.fg.b],
        });
    }
    out
}
```

- [ ] **Step 4: 接线**

`PaneRender` 加一个字段(`gpu.rs`):

```rust
pub struct PaneRender<'a> {
    pub geom: PaneGeom,
    pub snap: &'a GridSnapshot,
    pub focused: bool,
    /// F126:这个 pane 上组字中的拼音串。**只有焦点 pane 非空** ——
    /// 输入法一次只对一个 pane 生效。
    pub preedit: &'a str,
}
```

`quads_for_panes` 里,在每个 pane 的 `quads_for(...)` 之后追加 preedit 的 quad:

```rust
        if !p.preedit.is_empty() && p.snap.cursor.visible {
            let cells = crate::text::preedit_layout(
                p.snap.cols,
                p.snap.cursor.col,
                p.preedit,
            );
            out.extend(
                preedit_quads(&cells, origin, p.snap.cursor.row, cell_w, cell_h, defaults)
                    .into_iter()
                    .filter_map(|q| clamp_quad_to_bounds(q, bounds)),
            );
        }
```

光标位置:`quads_for` 画光标那段,列号改成走 `preedit_cursor_col` —— 因为
`quads_for` 拿不到 preedit,把这件事放在 `quads_for_panes` 里更简单:给 `quads_for`
加一个 `cursor_col_override: Option<u16>` 参数(**加在参数表末尾**),`quads_for_panes`
在有 preedit 时传 `Some(crate::text::preedit_cursor_col(p.snap.cols, p.snap.cursor.col, p.preedit))`,
其余情形传 `None`;`quads_for` 里 `let col = cursor_col_override.unwrap_or(snap.cursor.col);`。

改完把 Task 3 那个 `cursor_quads` 脚手架的最后补一个 `None`(**只改这一处**),
其余既有调用点同样补 `None`:

```bash
grep -rn "quads_for(" crates/mullion-app/src | grep -v "quads_for_panes"
```

`text.rs::prepare_panes` 里,在每个 pane 的行循环之后追加 preedit 的文字 run:

```rust
            // F126:组字中的拼音串。用与正文同一套 buffer 池,颜色取默认前景色
            // (它盖在自己铺的默认背景上,不跟随底下那格原本的 SGR 颜色 ——
            // 那格颜色可能恰好等于背景色,拼音就隐形了)。
            if !p.preedit.is_empty() && p.snap.cursor.visible {
                for c in crate::text::preedit_layout(p.snap.cols, p.snap.cursor.col, p.preedit) {
                    if n == bufs.len() {
                        bufs.push(Buffer::new(fs, metrics));
                    }
                    let buf = &mut bufs[n];
                    buf.set_metrics(fs, metrics);
                    let avail = p
                        .geom
                        .term_px
                        .w
                        .saturating_sub((f32::from(c.col) * cell_w) as u32)
                        .max(1) as f32;
                    buf.set_size(fs, Some(avail), Some(cell_h));
                    let s = c.ch.to_string();
                    let color = to_color(preedit_fg);
                    buf.set_rich_text(fs, [(s.as_str(), attrs.color(color))], attrs, Shaping::Advanced);
                    buf.shape_until_scroll(fs, false);
                    placements.push((pi, p.snap.cursor.row, c.col));
                    n += 1;
                }
            }
```

`preedit_fg` 从 `prepare_panes` 的新参数传入(调用方给 `theme::term_default_colors(&MULLION_DARK).fg`);
**一格一个 buffer**,与 `row_to_runs` 对 CJK 的处理同理(回退字体的 advance 与 `cell_w` 无关,
整串一个 buffer 会错位)。

`app.rs` 里构造 `PaneRender` 的地方(搜 `PaneRender {`)补 `preedit` 字段:

```rust
                preedit: if focused_here {
                    self.ime.preedit()
                } else {
                    ""
                },
```

`apply_ime_cursor_area` 里,列号改成 preedit 末尾:

```rust
        let cols = self
            .active_ws()
            .and_then(Workspace::focused)
            .map(|p| p.emulator.snapshot().cols)
            .unwrap_or(cur.col + 1);
        let col = crate::text::preedit_cursor_col(cols, cur.col, self.ime.preedit());
        let area = ime_cursor_area(g.term_px, (col, cur.row), cell_w, cell_h);
```

> 取 `cols` 时不要为此多跑一次 `snapshot()`(整格 Vec 分配,T3)。`Emulator` 已有
> `cursor()` 这条轻量路径;若拿不到列数,顺手在 `Emulator` 上加一个
> `pub fn cols(&self) -> u16 { self.term.columns() as u16 }`(`Dimensions` trait 已在作用域里)
> 并在 `mullion-term` 里配一条一行的单测。

`WindowEvent::Ime` 分支末尾已经有 `self.request_ui_redraw();`,preedit 变化会重绘,
不需要额外接线;但要在那里补一句 `self.apply_ime_cursor_area();`,否则候选框位置
要等下一次别的事件才更新。

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 | tail -20
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/gpu.rs crates/mullion-app/src/text.rs crates/mullion-app/src/app.rs crates/mullion-term/src/emulator.rs
git commit -m "feat(app): 组字中的拼音内联显示在光标处,带下划线 (F126)"
```

---

# 第三部分:F127 SFTP 文件类型图标

## Task 8: `classify` 判类纯函数

**Files:**
- Modify: `crates/mullion-app/src/ui/file_icon.rs`

**背景:** 现在四种线框全同色,一屏文件扫视时区分不出类型。判类、形状、颜色三件事
分开:这一步只做判类。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/ui/file_icon.rs` 的 `mod tests`:

```rust
    /// F127:扩展名 → 类型的表驱动判类。大小写不敏感(远端上 `.PNG` 常见)。
    #[test]
    fn classify_maps_extensions_to_kinds() {
        for (name, want) in [
            ("a.zip", IconKind::Archive),
            ("a.TAR.GZ", IconKind::Archive),
            ("a.tgz", IconKind::Archive),
            ("photo.png", IconKind::Image),
            ("photo.JPEG", IconKind::Image),
            ("main.rs", IconKind::Code),
            ("build.sh", IconKind::Code),
            ("Cargo.toml", IconKind::Code),
            ("README.md", IconKind::Doc),
            ("app.log", IconKind::Doc),
            ("setup.exe", IconKind::Exec),
            ("data.bin", IconKind::Other),
            ("Makefile", IconKind::Other),
        ] {
            assert_eq!(
                classify(EntryKind::File, name, 0o644),
                want,
                "{name} 判错了"
            );
        }
    }

    /// F127:目录 / 链接 由 `EntryKind` 决定,**扩展名说了不算** ——
    /// 一个叫 `backup.zip` 的目录仍然是目录。
    ///
    /// 自证会变红:把 `classify` 里 `EntryKind::Dir` 那一支删掉。
    #[test]
    fn entry_kind_wins_over_extension_for_dirs_and_links() {
        assert_eq!(classify(EntryKind::Dir, "backup.zip", 0o755), IconKind::Dir);
        assert_eq!(
            classify(EntryKind::Symlink, "latest.png", 0o777),
            IconKind::Link
        );
        assert_eq!(classify(EntryKind::Other, "ttyS0", 0o666), IconKind::Other);
    }

    /// F127:**扩展名优先于可执行位**。远端上的脚本本来就常带 `+x`,
    /// 反过来的话半屏 `.sh`/`.py` 会全变成齿轮,类型信息反而丢了。
    ///
    /// 自证会变红:把 `classify` 里可执行位那段判断挪到扩展名查表之前。
    #[test]
    fn extension_wins_over_the_execute_bit() {
        assert_eq!(classify(EntryKind::File, "run.sh", 0o755), IconKind::Code);
        assert_eq!(
            classify(EntryKind::File, "mullion", 0o755),
            IconKind::Exec,
            "没扩展名 + 有 x 位才判可执行"
        );
    }

    /// F127:点开头的隐藏文件不能把整个名字当扩展名 —— `.bashrc` 的
    /// 「扩展名」是空的,该落到 `Other`,而不是去查一个叫 `bashrc` 的扩展名。
    ///
    /// 自证会变红:把 `ext_of` 里那句 `if stem.is_empty() { return "" }` 删掉。
    #[test]
    fn dotfiles_have_no_extension() {
        assert_eq!(classify(EntryKind::File, ".bashrc", 0o644), IconKind::Other);
        assert_eq!(
            classify(EntryKind::File, ".config.json", 0o644),
            IconKind::Doc,
            "点开头但确实有扩展名的照常判"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib file_icon:: 2>&1 | tail -10
```

- [ ] **Step 3: 实现**

追加到 `crates/mullion-app/src/ui/file_icon.rs` 顶部(在 `outline` 之前):

```rust
/// F127:图标类型。比 `EntryKind` 细 —— 一屏全是同一个页角图标时,
/// 用户扫视找不到目标文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Dir,
    Archive,
    Image,
    Code,
    Doc,
    Exec,
    Link,
    Other,
}

/// 扩展名 → 类型。**唯一的一张表**,加类型只改这里。小写比对,调用方负责归一。
const EXT_TABLE: &[(&str, IconKind)] = &[
    ("zip", IconKind::Archive),
    ("tar", IconKind::Archive),
    ("gz", IconKind::Archive),
    ("tgz", IconKind::Archive),
    ("bz2", IconKind::Archive),
    ("xz", IconKind::Archive),
    ("7z", IconKind::Archive),
    ("rar", IconKind::Archive),
    ("png", IconKind::Image),
    ("jpg", IconKind::Image),
    ("jpeg", IconKind::Image),
    ("gif", IconKind::Image),
    ("bmp", IconKind::Image),
    ("svg", IconKind::Image),
    ("webp", IconKind::Image),
    ("ico", IconKind::Image),
    ("rs", IconKind::Code),
    ("py", IconKind::Code),
    ("sh", IconKind::Code),
    ("js", IconKind::Code),
    ("ts", IconKind::Code),
    ("c", IconKind::Code),
    ("h", IconKind::Code),
    ("cpp", IconKind::Code),
    ("go", IconKind::Code),
    ("java", IconKind::Code),
    ("rb", IconKind::Code),
    ("lua", IconKind::Code),
    ("toml", IconKind::Code),
    ("yaml", IconKind::Code),
    ("yml", IconKind::Code),
    ("md", IconKind::Doc),
    ("txt", IconKind::Doc),
    ("log", IconKind::Doc),
    ("json", IconKind::Doc),
    ("csv", IconKind::Doc),
    ("pdf", IconKind::Doc),
    ("doc", IconKind::Doc),
    ("docx", IconKind::Doc),
    ("exe", IconKind::Exec),
    ("msi", IconKind::Exec),
    ("bat", IconKind::Exec),
    ("cmd", IconKind::Exec),
];

/// 取小写扩展名。**点开头的隐藏文件没有扩展名**(`.bashrc` 不是「bashrc 类型」)。
fn ext_of(name: &str) -> String {
    let Some(ix) = name.rfind('.') else {
        return String::new();
    };
    if name[..ix].is_empty() {
        return String::new(); // `.bashrc`
    }
    name[ix + 1..].to_ascii_lowercase()
}

/// F127:一行该画哪种图标。
///
/// 优先级(顺序不可换):
/// 1. `EntryKind` 里的目录/链接/其他 —— 一个叫 `backup.zip` 的目录仍是目录。
/// 2. 扩展名查表 —— **优先于可执行位**:远端脚本本来就常带 `+x`,
///    反过来会让半屏 `.sh` 全变成齿轮。
/// 3. 可执行位(`mode` 的任意 x 位)。
/// 4. 其他。
pub fn classify(kind: EntryKind, name: &str, mode: u32) -> IconKind {
    match kind {
        EntryKind::Dir => return IconKind::Dir,
        EntryKind::Symlink => return IconKind::Link,
        EntryKind::Other => return IconKind::Other,
        EntryKind::File => {}
    }
    let ext = ext_of(name);
    if let Some((_, k)) = EXT_TABLE.iter().find(|(e, _)| *e == ext) {
        return *k;
    }
    if mode & 0o111 != 0 {
        return IconKind::Exec;
    }
    IconKind::Other
}
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib file_icon:: 2>&1 | tail -10
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/ui/file_icon.rs
git commit -m "feat(app): SFTP 图标判类纯函数(8 类,扩展名优先于 x 位) (F127)"
```

---

## Task 9: 8 类语义色进 theme

**Files:**
- Modify: `crates/mullion-app/src/theme.rs`

**背景:** 色板必须在 `theme.rs`(F80 定的:终端层和 egui 外壳共用一套 token),
`file_icon.rs` 只做「类型 → token」的映射。硬编 RGB 在 `ui/` 下的话,主题一换就失配。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/theme.rs` 的 `mod tests`:

```rust
    /// F127:8 类图标色在面板底色上必须达到 WCAG 1.4.11 的非文本对比度 3:1,
    /// 否则「颜色区分类型」这件事对暗色主题下的细线条图标不成立。
    /// 阈值与写法同 F62 的会话语义色。
    #[test]
    fn file_icon_colors_are_visible_on_the_panel() {
        let t = MULLION_DARK;
        for (name, c) in [
            ("dir", t.icon_dir),
            ("archive", t.icon_archive),
            ("image", t.icon_image),
            ("code", t.icon_code),
            ("doc", t.icon_doc),
            ("exec", t.icon_exec),
            ("link", t.icon_link),
            ("other", t.icon_other),
        ] {
            let ratio = contrast_ratio(c, t.panel_bg);
            assert!(ratio >= 3.0, "{name} 在面板底上只有 {ratio:.2}:1");
        }
    }

    /// F127:8 类颜色必须两两不同 —— 两类同色等于少一类。
    #[test]
    fn file_icon_colors_are_all_distinct() {
        let t = MULLION_DARK;
        let all = [
            t.icon_dir,
            t.icon_archive,
            t.icon_image,
            t.icon_code,
            t.icon_doc,
            t.icon_exec,
            t.icon_link,
            t.icon_other,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                assert_ne!(all[i], all[j], "第 {i} 类和第 {j} 类同色");
            }
        }
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib theme::tests::file_icon 2>&1 | tail -10
```

- [ ] **Step 3: 实现**

`Theme` 结构里加一段(放在「语义色」之后、「终端色」之前):

```rust
    // --- F127 文件类型图标色(§2.3 的延伸) ---
    /// 目录:蓝。与 `accent`(偏紫)刻意拉开,不然选中行的强调色和图标糊在一起。
    pub icon_dir: Rgb,
    /// 归档:橙。
    pub icon_archive: Rgb,
    /// 图片:绿。
    pub icon_image: Rgb,
    /// 代码:紫。
    pub icon_code: Rgb,
    /// 文档:灰蓝(最"安静"的一类,因为它最多)。
    pub icon_doc: Rgb,
    /// 可执行:黄。
    pub icon_exec: Rgb,
    /// 符号链接:青。
    pub icon_link: Rgb,
    /// 其他:中性灰。
    pub icon_other: Rgb,
```

`MULLION_DARK` 里给值(这组值在 `panel_bg = #14161f` 上的对比度已按 3:1 挑过,
改色前先跑上面那条测试):

```rust
    icon_dir: Rgb::new(0x6f, 0xa8, 0xff),
    icon_archive: Rgb::new(0xe0, 0x9a, 0x4a),
    icon_image: Rgb::new(0x5f, 0xc2, 0x8a),
    icon_code: Rgb::new(0xb0, 0x8c, 0xff),
    icon_doc: Rgb::new(0x9a, 0xa8, 0xc4),
    icon_exec: Rgb::new(0xd8, 0xc0, 0x52),
    icon_link: Rgb::new(0x58, 0xc0, 0xc8),
    // 比 `fg_dimmer`(0x8a90a8)亮一档:两者同值的话,「可操作的 other」
    // 和「不可操作的 other」在屏上一模一样,`color_for` 的灰化就失效了。
    icon_other: Rgb::new(0xa8, 0xae, 0xc4),
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib theme:: 2>&1 | tail -10
```

若某一类没过 3:1,把该色**整体提亮**(每个通道 +0x10)再跑,不要改阈值。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/theme.rs
git commit -m "feat(app): 8 类文件图标语义色进 theme(对比度 ≥ 3:1) (F127)"
```

---

## Task 10: 8 类形状 + 面板接线

**Files:**
- Modify: `crates/mullion-app/src/ui/file_icon.rs`
- Modify: `crates/mullion-app/src/ui/files_panel.rs:743`

- [ ] **Step 1: 写失败的测试**

把 `file_icon.rs` 里既有的两条测试(`every_icon_stays_inside_its_cell`、
`every_kind_looks_different_from_every_other_kind`)的 `ALL_KINDS` 换成 8 类:

```rust
    const ALL_KINDS: [IconKind; 8] = [
        IconKind::Dir,
        IconKind::Archive,
        IconKind::Image,
        IconKind::Code,
        IconKind::Doc,
        IconKind::Exec,
        IconKind::Link,
        IconKind::Other,
    ];
```

两条测试体里的 `outline(cell, kind)` 保持不变(签名从 `EntryKind` 改成 `IconKind`)。
再加一条:

```rust
    /// F127:颜色由类型决定,且**不可操作的行仍然整体变灰** —— 这是 D1 定的
    /// 闸门,不能因为加了类型色就丢掉,否则会出现「文字灰了图标还亮着」。
    ///
    /// 自证会变红:把 `color_for` 里 `if !usable` 那一支删掉。
    #[test]
    fn unusable_rows_stay_dim_even_with_type_colors() {
        let t = crate::theme::MULLION_DARK;
        assert_eq!(color_for(IconKind::Archive, true, &t), t.icon_archive);
        assert_eq!(color_for(IconKind::Archive, false, &t), t.fg_dimmer);
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib file_icon:: 2>&1 | tail -20
```

- [ ] **Step 3: 实现形状**

`outline` 的签名改成 `pub fn outline(rect: egui::Rect, kind: IconKind) -> Vec<Vec<egui::Pos2>>`,
match 扩成 8 支。目录/链接/其他沿用原来的形状(目录=带页签梯形、链接=页+箭头、
其他=菱形);新增五支:

```rust
        // 归档:盒子 + 一条捆带。
        IconKind::Archive => vec![
            vec![
                egui::pos2(l, t + r.height() * 0.2),
                egui::pos2(rt, t + r.height() * 0.2),
                egui::pos2(rt, b),
                egui::pos2(l, b),
                egui::pos2(l, t + r.height() * 0.2),
            ],
            vec![
                egui::pos2(l + r.width() * 0.4, t + r.height() * 0.2),
                egui::pos2(l + r.width() * 0.4, b),
            ],
            vec![
                egui::pos2(l + r.width() * 0.6, t + r.height() * 0.2),
                egui::pos2(l + r.width() * 0.6, b),
            ],
        ],
        // 图片:相框 + 里面一座山。
        IconKind::Image => vec![
            vec![
                egui::pos2(l, t),
                egui::pos2(rt, t),
                egui::pos2(rt, b),
                egui::pos2(l, b),
                egui::pos2(l, t),
            ],
            vec![
                egui::pos2(l + r.width() * 0.15, b - r.height() * 0.2),
                egui::pos2(l + r.width() * 0.4, t + r.height() * 0.45),
                egui::pos2(l + r.width() * 0.6, b - r.height() * 0.2),
            ],
        ],
        // 代码:一对尖括号。
        IconKind::Code => vec![
            vec![
                egui::pos2(l + r.width() * 0.4, t + r.height() * 0.15),
                egui::pos2(l, r.center().y),
                egui::pos2(l + r.width() * 0.4, b - r.height() * 0.15),
            ],
            vec![
                egui::pos2(rt - r.width() * 0.4, t + r.height() * 0.15),
                egui::pos2(rt, r.center().y),
                egui::pos2(rt - r.width() * 0.4, b - r.height() * 0.15),
            ],
        ],
        // 文档:页 + 三条文字线(与旧的「文件」形状区分开:那个是折角页)。
        IconKind::Doc => {
            let mut v = vec![vec![
                egui::pos2(l + r.width() * 0.15, t),
                egui::pos2(rt - r.width() * 0.15, t),
                egui::pos2(rt - r.width() * 0.15, b),
                egui::pos2(l + r.width() * 0.15, b),
                egui::pos2(l + r.width() * 0.15, t),
            ]];
            for i in 1..=3 {
                let y = t + r.height() * (0.2 * i as f32 + 0.1);
                v.push(vec![
                    egui::pos2(l + r.width() * 0.3, y),
                    egui::pos2(rt - r.width() * 0.3, y),
                ]);
            }
            v
        }
        // 可执行:一个朝右的三角(播放/运行)+ 底座。
        IconKind::Exec => vec![
            vec![
                egui::pos2(l + r.width() * 0.25, t + r.height() * 0.1),
                egui::pos2(rt - r.width() * 0.1, r.center().y),
                egui::pos2(l + r.width() * 0.25, b - r.height() * 0.3),
                egui::pos2(l + r.width() * 0.25, t + r.height() * 0.1),
            ],
            vec![egui::pos2(l, b), egui::pos2(rt, b)],
        ],
```

加颜色映射:

```rust
/// F127:类型 → 颜色。**不可操作的行恒 `fg_dimmer`**,与名称文字同源 ——
/// 两套判据会出现「文字灰了图标还亮着」这种自相矛盾的行(D1 定的闸门)。
pub fn color_for(kind: IconKind, usable: bool, t: &crate::theme::Theme) -> mullion_term::snapshot::Rgb {
    if !usable {
        return t.fg_dimmer;
    }
    match kind {
        IconKind::Dir => t.icon_dir,
        IconKind::Archive => t.icon_archive,
        IconKind::Image => t.icon_image,
        IconKind::Code => t.icon_code,
        IconKind::Doc => t.icon_doc,
        IconKind::Exec => t.icon_exec,
        IconKind::Link => t.icon_link,
        IconKind::Other => t.icon_other,
    }
}
```

`paint` 的签名跟着改成收 `IconKind`。

- [ ] **Step 4: 接线到面板**

`files_panel.rs:743` 那一段改成:

```rust
    // D1/F127:类型图标。判类看 `EntryKind` + 扩展名 + x 位,颜色跟
    // 可操作性同源(不可操作 → 与文字一样是 dim)。
    let icon_kind = crate::ui::file_icon::classify(
        e.kind,
        &e.name.display().to_string(),
        e.mode,
    );
    crate::ui::file_icon::paint(
        p,
        icon_rect(rect),
        icon_kind,
        theme::c32(crate::ui::file_icon::color_for(icon_kind, usable, t)),
    );
```

原来那个用于**文字**的 `fg` 变量保持不动(文字仍按 `fg_strong`/`fg`/`fg_dimmer` 三档)。

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 | tail -20
cargo clippy -p mullion-app --all-targets -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/ui/file_icon.rs crates/mullion-app/src/ui/files_panel.rs
git commit -m "feat(app): SFTP 文件图标扩到 8 类形状 + 语义色 (F127)"
```

---

# 第四部分:F128 断线检测与自动重连

> **顺序不能换。** Task 11(keepalive)是根因:russh 的默认配置一个 keepalive 都不发,
> 高延迟代理链路上 TCP 半开时**不会**产生 EOF,`rx` 永远不关,`pump` 里那条
> `TryRecvError::Disconnected` 分支永远不触发 —— 也就是说,今天这个 pane 连
> 「断开」都标不上,后面所有重连逻辑都是死代码。先把断开检测得出来,再谈重连。

## Task 11: SSH keepalive(断线检测的前提)

**Files:**
- Modify: `crates/mullion-ssh/src/session.rs:301`

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-ssh/src/session.rs` 的 `mod tests`:

```rust
    /// F128:**没有 keepalive 就没有断线检测**。高延迟代理链路上,链路中断
    /// 通常是「TCP 半开」——两端都不发 FIN/RST,连接看起来还在,只是字节
    /// 再也过不去。不周期性发包的话:`rx` 永远不关 → `Workspace::pump` 里
    /// 那条 `TryRecvError::Disconnected` 分支永远不触发 → pane 连
    /// `Disconnected` 都标不上,更谈不上重连。
    ///
    /// 10s × 3 = 最迟 30s 判定断开。再短的话,代理链路上偶发的几秒卡顿
    /// 会被误判成断线,把一条其实还活着的连接踢掉。
    ///
    /// 自证会变红:把 `client_config()` 的函数体换回 `client::Config::default()`。
    #[test]
    fn keepalive_is_configured_so_a_half_open_link_is_detected() {
        let c = client_config();
        assert_eq!(
            c.keepalive_interval,
            Some(std::time::Duration::from_secs(10)),
            "russh 默认是 None(一个包都不发)"
        );
        assert_eq!(c.keepalive_max, 3, "连丢 3 次判定断开");
    }

    /// F128:**不设 `inactivity_timeout`**。它是「多久没收到任何数据就断」,
    /// 而本项目的典型用法就是挂着一个几小时没输出的 tmux —— 设了它等于
    /// 定时踢掉空闲会话。链路死活由 keepalive 判定,不由「有没有内容」判定。
    ///
    /// 自证会变红:在 `client_config()` 里给 `inactivity_timeout` 赋任意值。
    #[test]
    fn idle_sessions_are_never_timed_out() {
        assert_eq!(
            client_config().inactivity_timeout,
            None,
            "挂着不动的 tmux 不是死连接"
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-ssh --lib keepalive_is_configured 2>&1 | tail -10
```

预期:`cannot find function 'client_config'`。

- [ ] **Step 3: 实现**

在 `crates/mullion-ssh/src/session.rs` 里加(放在 `establish` 之前):

```rust
/// F128:本项目的 russh 客户端配置。
///
/// 抽成函数是为了能单测 —— `establish` 要真网络,配置对不对在无头环境里
/// 只能这样验。默认值不能用:`client::Config::default()` 的
/// `keepalive_interval` 是 `None`,详见 `keepalive_is_configured_so_a_half_open_link_is_detected`。
pub fn client_config() -> client::Config {
    client::Config {
        // 10s 一个包,连丢 3 次(≈30s)判定断开。
        keepalive_interval: Some(std::time::Duration::from_secs(10)),
        keepalive_max: 3,
        // 显式写出来:空闲不是断线,挂几小时的 tmux 是本项目的常态用法。
        inactivity_timeout: None,
        ..client::Config::default()
    }
}
```

`session.rs:301` 改成:

```rust
    let config = Arc::new(client_config());
```

同文件里若还有别处构造 `client::Config`(跳板那一跳),一并换过来:

```bash
grep -n "client::Config::default()" crates/mullion-ssh/src/*.rs
```

**跳板那几跳也要 keepalive** —— 中间跳死了,末端一样收不到字节。

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-ssh 2>&1 | tail -10
```

- [ ] **Step 5: 真机验证(可选但强烈建议)**

```bash
MULLION_LIVE=1 MULLION_LIVE_HOST=<真机> MULLION_LIVE_USER=<用户> MULLION_LIVE_KEY=<私钥> \
  cargo test -p mullion-ssh --test live -- --ignored 2>&1 | tail -20
```

预期:live 用例照常通过(keepalive 不该影响正常连接)。

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-ssh/src/session.rs
git commit -m "fix(ssh): 开 keepalive(10s×3),半开链路才检测得出断线 (F128)

这是「断线不重连」的根因:russh 默认一个 keepalive 都不发,TCP 半开时
rx 永远不关,pump 里的 Disconnected 分支永远不触发。"
```

---

## Task 12: `Reconnecting` 状态 + rx 关闭的成因判别

**Files:**
- Modify: `crates/mullion-app/src/shell/workspace/mod.rs`
- Modify: `crates/mullion-app/src/shell/workspace/preset.rs`
- Modify: `crates/mullion-app/src/ui/pane_title.rs`

**背景:** `rx` 关闭有**两个**成因,混为一谈会出大事:

| 成因 | `handle.is_closed()` | 该做什么 |
|---|---|---|
| 用户在远端敲了 `exit` / 远端进程退出 | `false`(SSH 连接还活着) | **绝不重连** —— 否则用户永远退不出登录 |
| 链路死了(keepalive 判定) | `true` | 重连 |

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/shell/workspace/mod.rs` 的 `mod tests`:

```rust
    /// F128:`rx` 关了有两个成因,**判反了两边都是灾难**:
    /// - 用户敲 `exit`(连接还活着)却去重连 → 用户永远退不出登录,
    ///   每次 exit 都被自动拉回来。
    /// - 链路死了却当成正常退出 → 就是今天的现状,永远不重连。
    ///
    /// 判据是 SSH **连接**(不是 channel)还在不在。
    ///
    /// 自证会变红:把 `rx_closed_action` 的函数体改成恒返回
    /// `RxClosed::Reconnect`。
    #[test]
    fn a_closed_rx_means_reconnect_only_if_the_transport_died() {
        assert_eq!(rx_closed_action(true), RxClosed::Reconnect, "链路死了");
        assert_eq!(
            rx_closed_action(false),
            RxClosed::UserExited,
            "连接还活着 = 远端 shell 自己退了,不许重连"
        );
    }

    /// F128:`pump` 必须按成因分别置位。`Reconnecting` 与 `Disconnected`
    /// 的差别是「等一下会自己回来」vs「不会再回来了」,标题条的点、
    /// Ctrl+D 的语义、减屏时先关谁,三处都看它。
    #[tokio::test]
    async fn pump_marks_reconnecting_when_the_transport_died() {
        let (mut ws, probes) = ws_with(1);
        // 造「链路死了」:丢掉发送端(rx 关闭)+ 让判据报 false。
        drop(probes);
        ws.link_alive = |_, _| false;
        ws.pump(0);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Reconnecting);
    }

    /// 反面:同样是 rx 关了,链路还活着就只是「远端 shell 退了」。
    /// 这一条与上一条**必须成对**——只有一条的话,把 `rx_closed_action` 写成
    /// 恒返回某一个值也能过。
    #[tokio::test]
    async fn pump_marks_disconnected_when_the_remote_shell_exited() {
        let (mut ws, probes) = ws_with(1);
        drop(probes);
        ws.link_alive = |_, _| true;
        ws.pump(0);
        assert_eq!(ws.pane(PaneId(1)).unwrap().status, PaneStatus::Disconnected);
    }
```

> 测试钩子:`ws_with` 造出来的 `Workspace` 里 `hosts` 是空的(它不需要真连接),
> 所以判据不能写成「查 `hosts[ix]`」那种在测试里无法翻转的形式。**采用**:
> `Workspace` 上加一个 `pub link_alive: fn(&[HostConn], usize) -> bool` 字段,
> 生产默认查 `handle.is_closed()`,测试里直接赋一个常量闭包。fn 指针是 `Copy`,
> 不引入生命周期/装箱开销,也不污染生产路径。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib workspace::tests::a_closed_rx 2>&1 | tail -10
```

- [ ] **Step 3: 实现**

`workspace/mod.rs`:

```rust
/// pane 的连接状态(§6.3)。断开的 pane 内容保留、可滚可复制,只是不再收发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Live,
    /// F128:链路死了,正在自动退避重连。**内容保留**——重连成功后接着往下写,
    /// 用户滚回去还能看到断线前的输出。
    Reconnecting,
    /// 不会自己回来了:远端 shell 自己退了(用户敲了 `exit`),或重试到顶。
    Disconnected,
}

/// F128:`rx` 关闭该怎么处置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxClosed {
    /// 链路死了,自动重连。
    Reconnect,
    /// 远端 shell 自己退了(`exit`),SSH 连接还活着 —— **绝不重连**,
    /// 否则用户永远退不出登录。
    UserExited,
}

/// F128:判据只有一条 —— SSH **连接**(不是 channel)还在不在。
/// channel 关了而连接还在 = 远端进程退了;连接也没了 = 链路死了。
pub fn rx_closed_action(transport_alive: bool) -> RxClosed {
    if transport_alive {
        RxClosed::UserExited
    } else {
        RxClosed::Reconnect
    }
}
```

`Workspace` 加字段 + 默认实现:

```rust
/// F128:`hosts[ix]` 这条连接的传输层还活着没有。
///
/// 抽成 fn 指针字段是为了**能在无头测试里翻转** —— 直接查
/// `handle.is_closed()` 的话,"链路死了"这个状态在测试里根本造不出来
/// (测试用的 `Workspace` 连 `HostConn` 都没有)。
/// 查不到 host 时返回 `true`(当成"连接还在"):那是异常状态,
/// 宁可不重连,也不要对着一条不存在的连接无限重拨。
pub fn default_link_alive(hosts: &[HostConn], ix: usize) -> bool {
    // 写成 match 而不是 `map_or`/`is_none_or`:后两者在不同 clippy 版本里
    // 会互相建议对方(`-D warnings` 下就是编不过),match 两边都不挑刺。
    match hosts.get(ix) {
        None => true,
        Some(h) => !h.handle.is_closed(),
    }
}
```

```rust
    /// F128:判据见 `default_link_alive`。测试里可替换。
    pub link_alive: fn(&[HostConn], usize) -> bool,
```

构造 `Workspace` 的地方给默认值:`link_alive: default_link_alive,`。

`pump` 里,在 `for p in &mut self.panes` **之前**取出两样(`self.panes` 与
`self.hosts` 是不同字段,分别借用编得过;`fn` 指针是 `Copy`):

```rust
        let link_alive = self.link_alive;
        let hosts = &self.hosts;
        for p in &mut self.panes {
```

那条分支改成:

```rust
                    Err(TryRecvError::Disconnected) => {
                        // §6.3:内容保留(可滚可复制),只是不再收发。
                        // F128:关掉的成因决定要不要重连,判据见 `rx_closed_action`。
                        p.status = match rx_closed_action(link_alive(hosts, p.host_ix)) {
                            RxClosed::Reconnect => PaneStatus::Reconnecting,
                            RxClosed::UserExited => PaneStatus::Disconnected,
                        };
                        break;
                    }
```

> 既有测试 `pump_marks_pane_disconnected_when_channel_closes`(约 `mod.rs:734`)
> 用的 `Workspace` 没有 `hosts`,按上面的「查不到 host = 连接还在」规则它仍然
> 落到 `Disconnected`,保持绿。**跑一遍确认**,变红就是规则写反了。

`preset.rs` 的减屏优先级改成穷尽 match(「列举式门控在加档时必然漏」已经踩中过四次):

```rust
    // 减屏:按「关掉的代价」从小到大关。**穷尽 match**——加状态时这里
    // 编译报错,而不是新状态悄悄一个都关不掉(那样 close 会凑不够 extra 个,
    // 减屏静默失效)。
    fn close_priority(s: PaneStatus) -> u8 {
        match s {
            PaneStatus::Disconnected => 0, // 已经死透,先关
            PaneStatus::Reconnecting => 1, // 还有救,但没内容在动
            PaneStatus::Live => 2,         // 最后才关活的
        }
    }
    let mut ranked: Vec<(u8, usize, PaneId)> = current
        .iter()
        .enumerate()
        .map(|(i, (id, s))| (close_priority(*s), usize::MAX - i, *id))
        .collect();
    // 同优先级里按几何逆序(右下角先走)——`usize::MAX - i` 就是逆序键。
    ranked.sort_by_key(|(p, rev, _)| (*p, *rev));
    let close: Vec<PaneId> = ranked.into_iter().take(extra).map(|(_, _, id)| id).collect();
```

`pane_title.rs` 的两处 match 补一支:

```rust
                            PaneStatus::Live => t.ok,
                            // F128:重连中 = 黄色(还有救),与 Disconnected 的灰
                            // 明确区分:用户看一眼就知道要不要自己动手。
                            PaneStatus::Reconnecting => t.warn,
                            PaneStatus::Disconnected => t.fg_dim,
```

`pane_title.rs:74` 那个 `if status == PaneStatus::Disconnected`(标题文字变灰的判据)
**保持只判 `Disconnected`** —— 重连中的 pane 还有救,不该整条标题灰掉。

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 | tail -20
```

预期:编译报错会指出所有没穷尽的 `match PaneStatus` —— **逐个看一遍再补**,
别无脑加 `_ =>`(那正是这条 Task 要根除的东西)。

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/shell/workspace/mod.rs crates/mullion-app/src/shell/workspace/preset.rs crates/mullion-app/src/ui/pane_title.rs
git commit -m "feat(app): 区分「远端 exit」与「链路死了」,加 Reconnecting 状态 (F128)

减屏优先级改成穷尽 match,新状态漏判会编译报错而不是静默失效。"
```

---

## Task 13: `reattach_pane`——换 channel 但**保留内容**

**Files:**
- Modify: `crates/mullion-app/src/app.rs`(`rehost_pane` 旁边)

**背景:** 既有 `rehost_pane` 会**重建 emulator**(换机器,旧内容属于上一台,该丢)。
重连要的正相反:**同一台机器,内容必须留着**(用户拍板:保留旧屏内容 + 重跑登录后自动化)。
两者其余字段的处理完全相同,抽一个私有 helper,免得将来只改一处。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/app.rs` 的 `mod tests`:

```rust
    /// F128:重连换的是 channel,**不是机器** —— 屏上内容必须原样留着
    /// (用户拍板:保留旧屏内容)。重建 emulator 的话,断线前那一屏
    /// (往往正是他想看的报错)当场消失。
    ///
    /// 自证会变红:把 `reattach_pane` 改成调 `rehost_pane`。
    #[test]
    fn reattach_keeps_the_screen_but_rehost_wipes_it() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        let (mut ws, _p) = ws_with(1);
        let gen = ws.generation();
        ws.pane_mut(PaneId(1))
            .unwrap()
            .emulator
            .feed(b"before-the-drop");
        let (pty, rx) = fresh_pipe();
        assert!(reattach_pane(&mut ws, PaneId(1), gen, 0, pty, rx));
        let text: String = ws
            .pane(PaneId(1))
            .unwrap()
            .emulator
            .snapshot()
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            text.contains("before-the-drop"),
            "重连不该抹掉断线前的内容,实际:{text:?}"
        );
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().status,
            crate::shell::workspace::PaneStatus::Live
        );

        // 对照组:换机器那条路必须**抹掉**内容(否则上一台机器的输出会
        // 挂在新机器的屏上,用户完全分不清哪些字是谁说的)。
        let (pty, rx) = fresh_pipe();
        assert!(rehost_pane(&mut ws, PaneId(1), gen, 0, pty, rx));
        let text: String = ws
            .pane(PaneId(1))
            .unwrap()
            .emulator
            .snapshot()
            .cells
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(!text.contains("before-the-drop"), "换机器该重建 emulator");
    }

    /// F128:换了 channel 之后必须逼出一次 `window_change`(T4)。
    /// 新 channel 是 80x24,不重发的话远端 tmux 里的 TUI 按 80 列排版,
    /// 全屏 TUI 直接错行 —— 这是 T4 的原样复发。
    ///
    /// 自证会变红:把 `reattach_pane` 里的 `p.last_grid = (0, 0);` 删掉。
    #[test]
    fn reattach_forces_a_window_change() {
        use crate::shell::workspace::tests_support::{fresh_pipe, ws_with};
        let (mut ws, _p) = ws_with(1);
        let gen = ws.generation();
        ws.pane_mut(PaneId(1)).unwrap().last_grid = (120, 40);
        let (pty, rx) = fresh_pipe();
        reattach_pane(&mut ws, PaneId(1), gen, 0, pty, rx);
        assert_eq!(
            ws.pane(PaneId(1)).unwrap().last_grid,
            (0, 0),
            "不复位的话下一帧 apply_geometry 认为尺寸没变,不发 window_change(T4)"
        );
    }
```

> `tests_support`:`ws_with`(`workspace/mod.rs:494`,返回 `(Workspace, Vec<Probe>)`)
> 和它依赖的 `fake_pane`/`Probe` 现在在 `workspace::tests` 里,跨模块用不了。
> 把这三样**原样**挪到 `workspace` 下新建的 `#[cfg(test)] pub mod tests_support`,
> 再加一个 `fresh_pipe()`:
>
> ```rust
>     /// 一条崭新的、没接任何东西的 PTY 管道 —— 换 channel 那两条路径要拿它当
>     /// 「新开好的 channel」。
>     pub fn fresh_pipe() -> (Box<dyn super::PtyWriter>, tokio::sync::mpsc::Receiver<Vec<u8>>) {
>         let (p, probe) = fake_pane(999);
>         let rx = p.rx;
>         std::mem::forget(probe); // 发送端留着,免得 rx 立刻变成 Disconnected
>         (p.pty, rx)
>     }
> ```
>
> `fake_pane` 的实际返回结构以源码为准(它构造整个 `PaneState`),按上面的思路
> 把 `pty`/`rx` 拆出来即可。**只挪不改**,挪完先跑一遍 workspace 的测试确认还绿。

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib reattach_keeps_the_screen 2>&1 | tail -10
```

- [ ] **Step 3: 实现**

`app.rs` 里把 `rehost_pane` 拆成:

```rust
/// 换 channel 时两条路径(换机器 / 重连)**共同**要做的事。
///
/// 抽出来是因为漏掉其中任何一条都会产生难查的 bug:`last_grid` 漏了是 T4
/// (远端按 80 列排版),`saw_first_byte` 漏了是自动化在一条还没说话的 channel
/// 上开跑,`pacer` 漏了是上一条 channel 没收口的同步块把新内容一直攒着(T2)。
fn swap_pane_channel(
    p: &mut crate::shell::workspace::PaneState,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
) {
    p.host_ix = host_ix;
    // 旧的 `pty`/`rx` 在这两句赋值里被 Drop —— Drop 即关掉上一条 channel,
    // 不留孤儿。
    p.pty = pty;
    p.rx = rx;
    p.pacer = SyncFramePacer::new();
    p.status = crate::shell::workspace::PaneStatus::Live;
    p.saw_first_byte = false;
    p.last_grid = (0, 0);
}

fn rehost_pane(
    ws: &mut Workspace,
    id: PaneId,
    generation: u64,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
) -> bool {
    if !pane_still_wanted(ws, id, generation) {
        return false;
    }
    let Some(p) = ws.pane_mut(id) else {
        return false;
    };
    // 换机器:旧内容属于上一台,连同它嗅出来的 cwd/tmux 一起丢。
    let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
    let d = theme::term_default_colors(&MULLION_DARK);
    emulator.set_default_colors(d.fg, d.bg);
    p.emulator = emulator;
    p.cwd = None;
    p.tmux = None;
    swap_pane_channel(p, host_ix, pty, rx);
    true
}

/// F128:断线重连之后把 pane 挂到**新开的 channel** 上。
///
/// 与 `rehost_pane` 的唯一差别:**保留 `emulator`**(以及它嗅出来的 `cwd`/`tmux`)。
/// 还是同一台机器、同一个用户,断线前那一屏内容是用户想看的东西
/// (往往正是导致断线的那条报错),重建等于当场抹掉。`host_ix` 仍要传:
/// 重连会往 `ws.hosts` 里 push 一条新的 `HostConn`(旧的那条连接已经死了)。
fn reattach_pane(
    ws: &mut Workspace,
    id: PaneId,
    generation: u64,
    host_ix: usize,
    pty: Box<dyn crate::shell::workspace::PtyWriter>,
    rx: Receiver<Vec<u8>>,
) -> bool {
    if !pane_still_wanted(ws, id, generation) {
        return false;
    }
    let Some(p) = ws.pane_mut(id) else {
        return false;
    };
    swap_pane_channel(p, host_ix, pty, rx);
    true
}
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app 2>&1 | tail -20
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/shell/workspace/mod.rs
git commit -m "feat(app): reattach_pane —— 换 channel 但保留屏内容 (F128)

与 rehost_pane 共用 swap_pane_channel,last_grid 复位守护 T4。"
```

---

## Task 14: 重连调度纯函数

**Files:**
- Create: `crates/mullion-app/src/reconnect.rs`
- Modify: `crates/mullion-app/src/lib.rs`(挂 `mod reconnect;`)

**背景:** 「什么时候、为哪条连接、发起第几次重拨」这件事必须是纯函数,否则只能靠
真断线来验。退避表复用 `mullion_ssh::tunnel::backoff_delay`(隧道已经在用同一套)。

- [ ] **Step 1: 写失败的测试**

新建 `crates/mullion-app/src/reconnect.rs`,先写测试:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// F128:**按 host 分组**。adr-009 说一条 SSH 连接承载多个 pane,
    /// 4 块分屏挂在同一台机器上时,链路一死是 4 块一起报 `Reconnecting` ——
    /// 每块各拨一次就是 4 条连接、4 次认证,对高延迟代理链路是纯浪费,
    /// 而且远端会看到 4 次登录。
    ///
    /// 自证会变红:把 `hosts_to_redial` 里的去重(`seen`)删掉。
    #[test]
    fn all_panes_of_one_host_share_a_single_redial() {
        let panes = [
            (PaneId(1), 0usize, PaneStatus::Reconnecting),
            (PaneId(2), 0, PaneStatus::Reconnecting),
            (PaneId(3), 1, PaneStatus::Reconnecting),
            (PaneId(4), 0, PaneStatus::Live),
        ];
        assert_eq!(hosts_to_redial(&panes, &[]), vec![0, 1]);
    }

    /// F128:已经在途的那条不再发起 —— 不然每一帧都会再拨一次
    /// (帧循环 60fps,一秒六十条连接)。
    ///
    /// 自证会变红:把 `hosts_to_redial` 里 `in_flight.contains` 那一句删掉。
    #[test]
    fn a_redial_already_in_flight_is_not_started_again() {
        let panes = [(PaneId(1), 0usize, PaneStatus::Reconnecting)];
        assert!(hosts_to_redial(&panes, &[0]).is_empty());
    }

    /// F128:只有 `Reconnecting` 才拨。`Disconnected`(用户敲了 `exit`)
    /// 去拨的话,用户永远退不出登录。
    #[test]
    fn disconnected_panes_are_never_redialed() {
        let panes = [
            (PaneId(1), 0usize, PaneStatus::Disconnected),
            (PaneId(2), 1, PaneStatus::Live),
        ];
        assert!(hosts_to_redial(&panes, &[]).is_empty());
    }

    /// F128:退避表直接用隧道那套(`mullion_ssh::tunnel::backoff_delay`),
    /// 不另写一份 —— 两套退避策略会在同一条链路抖动时互相打架,而且用户
    /// 看到的"多久重试一次"会因为断的是隧道还是终端而不同。
    ///
    /// 自证会变红:把 `delay_for` 改成返回一个常量 `Duration`。
    #[test]
    fn backoff_is_the_same_table_as_tunnels() {
        for attempt in 0..8 {
            assert_eq!(
                delay_for(attempt),
                mullion_ssh::tunnel::backoff_delay(attempt),
                "第 {attempt} 次的退避不一致"
            );
        }
    }

    /// F128:退避到顶(`backoff_delay` 返回 `None`)= 放弃,pane 落到
    /// `Disconnected`,由用户自己决定重连还是关掉。一直重试到天荒地老的话,
    /// 一台已经拆机的服务器会让客户端永远有一个后台任务在跑。
    #[test]
    fn giving_up_turns_into_a_plain_disconnect() {
        let last = (0..).find(|a| delay_for(*a).is_none()).expect("总会有上限");
        assert!(delay_for(last).is_none());
        assert_eq!(status_after_failure(last), PaneStatus::Disconnected);
        assert_eq!(status_after_failure(0), PaneStatus::Reconnecting);
    }

    /// F128:屏内提示是**一行**,喂进 emulator 当普通输出。做倒计时的话要在
    /// 帧循环里再引一个 deadline,正是 spec §1 修订一要避免的东西
    /// (同 `automation_status` 不做定时淡出)。
    #[test]
    fn the_in_screen_notice_is_one_line_of_plain_output() {
        let s = String::from_utf8(notice_bytes(2, std::time::Duration::from_secs(4))).unwrap();
        assert!(s.starts_with("\r\n"), "另起一行,不覆盖远端最后那行输出");
        assert!(s.ends_with("\r\n"));
        assert_eq!(s.matches('\n').count(), 2, "只有一行正文");
        assert!(s.contains("第 2 次"), "实际:{s:?}");
        assert!(s.contains("4"), "要告诉用户等多久,实际:{s:?}");
    }
}
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib reconnect:: 2>&1 | tail -10
```

预期:`file not found for module` 或 `cannot find function`。

- [ ] **Step 3: 实现**

`crates/mullion-app/src/reconnect.rs` 的正文(放在 `mod tests` 之前):

```rust
//! F128:断线自动重连的**判据层**。
//!
//! 这里只有纯函数 —— 拨号本身要真网络,但「拨不拨、拨哪条、等多久、
//! 什么时候放弃」全都能在无头容器里单测。

use std::time::Duration;

use mullion_core::layout::PaneId;

use crate::shell::workspace::PaneStatus;

/// 这一帧要为哪些 host 发起重拨。
///
/// **按 host 去重**:adr-009 下一条连接承载多个 pane,4 块分屏一起断时
/// 拨 4 次等于 4 次认证 + 远端 4 条登录记录。
/// `in_flight` 是已经在拨的 host,重复发起会让帧循环每秒拨几十次。
pub fn hosts_to_redial(
    panes: &[(PaneId, usize, PaneStatus)],
    in_flight: &[usize],
) -> Vec<usize> {
    let mut seen: Vec<usize> = Vec::new();
    for (_, host_ix, status) in panes {
        if *status != PaneStatus::Reconnecting {
            continue;
        }
        if in_flight.contains(host_ix) || seen.contains(host_ix) {
            continue;
        }
        seen.push(*host_ix);
    }
    seen
}

/// 第 `attempt` 次重试前该等多久。`None` = 到顶,放弃。
///
/// 直接转发隧道那套表(`mullion_ssh::tunnel::backoff_delay`):两套退避会在
/// 同一条链路抖动时互相打架,用户看到的"多久重试一次"也会因为断的是隧道还是
/// 终端而不同。
pub fn delay_for(attempt: u32) -> Option<Duration> {
    mullion_ssh::tunnel::backoff_delay(attempt)
}

/// 第 `attempt` 次重拨失败之后,pane 该是什么状态。
/// 退避到顶就落回 `Disconnected`,由用户自己决定重连还是关掉 —— 一直重试
/// 到天荒地老的话,一台已经拆掉的服务器会让客户端永远挂着一个后台任务。
pub fn status_after_failure(attempt: u32) -> PaneStatus {
    match delay_for(attempt) {
        Some(_) => PaneStatus::Reconnecting,
        None => PaneStatus::Disconnected,
    }
}

/// 喂进 emulator 的那一行屏内提示(§7.3)。
///
/// **不做倒计时**:那要在帧循环里再引一个 deadline,正是 spec §1 修订一要
/// 避免的东西(同 `automation_status` 不做定时淡出)。前后各一个 `\r\n`:
/// 前面那个保证不覆盖远端最后一行输出,后面那个让重连成功后的新输出从行首开始。
pub fn notice_bytes(attempt: u32, delay: Duration) -> Vec<u8> {
    format!(
        "\r\n[Mullion] 连接已断开,第 {attempt} 次重连将在 {} 秒后开始…\r\n",
        delay.as_secs().max(1)
    )
    .into_bytes()
}
```

`crates/mullion-app/src/lib.rs` 里挂上(按字母序插进既有的 `mod` 列表):

```rust
pub mod reconnect;
```

- [ ] **Step 4: 跑测试确认绿**

```bash
cargo test -p mullion-app --lib reconnect:: 2>&1 | tail -10
```

- [ ] **Step 5: 提交**

```bash
git add crates/mullion-app/src/reconnect.rs crates/mullion-app/src/lib.rs
git commit -m "feat(app): 重连判据层(按 host 去重/复用隧道退避表/放弃即断开) (F128)"
```

---

## Task 15: 重连接线

**Files:**
- Modify: `crates/mullion-app/src/app.rs`

**背景:** 这是本 plan 最长的一步,但每一块都有既成模板:发起 = `spawn_rehost`,
回收 = `PaneRehosted`,自动化 = `start_automation`。差别只有三处:**用
`tab.last_cfg` 而不是回头查库**、**换挂走 `reattach_pane`**、**成功后清 SFTP**。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/app.rs` 的 `mod tests`(源码级接线守护 —— 真断线在
无头容器里造不出来;**锚点带行首缩进**,不带的话会匹配到测试自己那一行,恒绿):

```rust
    /// **接线守护 / F128**:重连的凭据必须来自 `tab.last_cfg`,**不许回头查库**。
    /// 查库的话,用户在断线期间改了这条会话(换了端口/密钥),重连就会拨到
    /// 一个他没同意过的地方去;会话被删掉时更是直接连不上。
    /// 理由同 `PendingRehost` / `automation_template`。
    ///
    /// 自证会变红:把 `spawn_reconnect` 里的 `last_cfg` 换成 `store.dial_plan_for(..)`。
    #[test]
    fn reconnect_uses_the_cfg_frozen_at_connect_time() {
        let src = include_str!("app.rs");
        let body = fn_body(src, "    fn spawn_reconnect(");
        assert!(
            body.contains("last_cfg"),
            "重连没用连接时定死的 cfg —— 断线期间改过会话就会拨到别处"
        );
        assert!(
            !body.contains("dial_plan_for"),
            "重连回头查库了 —— 见 PendingRehost 的文档"
        );
    }

    /// **接线守护 / F128**:重连成功要走 `reattach_pane`(保留内容),
    /// 不是 `rehost_pane`(重建 emulator)。走错的现象是「重连之后屏是空的」,
    /// 而用户最想看的恰恰是断线前那一屏。
    ///
    /// 自证会变红:把 `PaneReconnected` 分支里的 `reattach_pane(` 改成 `rehost_pane(`。
    #[test]
    fn reconnect_reattaches_instead_of_rehosting() {
        let src = include_str!("app.rs");
        let arm = src
            .split("            UserEvent::PaneReconnected {")
            .nth(1)
            .expect("找不到 PaneReconnected 分支");
        let arm = &arm[..arm.find("\n            UserEvent::").unwrap_or(arm.len())];
        assert!(arm.contains("reattach_pane("), "重连必须保留屏内容");
        assert!(!arm.contains("rehost_pane("), "走了换机器那条路,内容会被抹掉");
    }

    /// **接线守护 / F128**:重连成功后要重跑登录后自动化 —— 用户拍板的规则,
    /// 否则 tmux 不会 attach,断线前那个 Claude Code 会话回不来
    /// (这正是整个 F128 要解决的场景)。
    ///
    /// 自证会变红:把 `PaneReconnected` 分支里的 `start_automation` 那句删掉。
    #[test]
    fn reconnect_reruns_post_login_automation() {
        let src = include_str!("app.rs");
        let arm = src
            .split("            UserEvent::PaneReconnected {")
            .nth(1)
            .expect("找不到 PaneReconnected 分支");
        let arm = &arm[..arm.find("\n            UserEvent::").unwrap_or(arm.len())];
        assert!(
            arm.contains("self.start_automation("),
            "没重跑登录后命令 —— tmux 不 attach,Claude Code 会话回不来"
        );
    }

    /// **接线守护 / F128**:重连成功要把这个标签的 SFTP 侧栏运行态清掉。
    /// 旧 `SftpClient` 挂在**已经死掉的那条连接**上,留着的话侧栏每次操作都
    /// 静默失败,而用户看到的是「文件面板卡住了」。
    ///
    /// 自证会变红:把 `PaneReconnected` 分支里 `t.sftp = None;` 那句删掉。
    #[test]
    fn reconnect_drops_the_dead_sftp_client() {
        let src = include_str!("app.rs");
        let arm = src
            .split("            UserEvent::PaneReconnected {")
            .nth(1)
            .expect("找不到 PaneReconnected 分支");
        let arm = &arm[..arm.find("\n            UserEvent::").unwrap_or(arm.len())];
        assert!(arm.contains("t.sftp = None;"), "死掉的 SftpClient 必须丢掉");
        assert!(arm.contains("t.sftp_home = None;"));
    }
```

若 `fn_body` 这个脚手架还不存在,加到同一个 `mod tests`:

```rust
    /// 取某个函数的函数体源码。**`marker` 必须带行首缩进**——不带的话
    /// `include_str!` 出来的源码里会先匹配到测试自己写的那个字符串字面量,
    /// 断言就变成了「测试自我匹配」,永远绿(本项目已实证的第五类恒绿模式)。
    fn fn_body<'a>(src: &'a str, marker: &str) -> &'a str {
        assert!(
            marker.starts_with("    "),
            "锚点必须带行首缩进,否则测试自我匹配恒绿"
        );
        let after = src.split(marker).nth(1).unwrap_or_else(|| panic!("找不到 {marker}"));
        &after[..after.find("\n    }\n").expect("找不到函数结尾")]
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib reconnect_uses_the_cfg 2>&1 | tail -10
```

预期:`panicked at '找不到     fn spawn_reconnect('`。

- [ ] **Step 3: 加事件变体**

`UserEvent` 里加两条(挨着 `PaneRehosted` 写):

```rust
    /// F128:一次断线重连拨通了。**跟 `PaneRehosted` 分开**:那条的语义是
    /// 「把 pane 改挂到另一台机器」(要重建 emulator),这条是「同一台机器
    /// 换一条 channel」(必须保留 emulator)。挤在一起只能靠运行时标志判别,
    /// 而走错的后果是把用户断线前那一屏抹掉。
    PaneReconnected {
        generation: u64,
        /// 这条连接原来的 `host_ix`——重连成功后 `ws.hosts` 会 push 一条新的,
        /// 挂在旧 ix 上的**每一块** pane 都要跟着换过去(adr-009:一条连接
        /// 多块 pane)。
        host_ix: usize,
        handle: Arc<SshConnection>,
        /// 每块 pane 一条新 channel,顺序与 `panes` 对齐。
        channels: Vec<(PaneId, SshSession, Receiver<Vec<u8>>)>,
    },
    /// F128:一次重连没拨通。`attempt` 是刚失败的这次的序号,决定下次等多久
    /// (以及要不要放弃),判据在 `crate::reconnect`。
    PaneReconnectErr {
        generation: u64,
        host_ix: usize,
        attempt: u32,
        msg: String,
    },
```

`App` 上加在途表:

```rust
    /// F128:正在重拨的连接 `(generation, host_ix, 已失败次数)`。
    /// 存在这张表里的 host 这一帧不再发起 —— 帧循环 60fps,不去重就是
    /// 一秒六十条连接(判据在 `reconnect::hosts_to_redial`)。
    reconnecting: Vec<(u64, usize, u32)>,
```

构造 `App` 的地方给初值 `reconnecting: Vec::new()`。`PromptingPolicy::new` 的三个
参数与 `spawn_rehost` 里**逐字一致**(第三个 `true` 是"后台策略":指纹变了当场停,
不在重连途中弹窗要用户拍板 —— 断线正是中间人最好下手的时机)。

- [ ] **Step 4: 发起重连**

在 `RedrawRequested` 里 `ws.pump(now)` 之后加一句 `self.drive_reconnects();`,
并实现它:

```rust
    /// F128:这一帧该发起哪些重拨。挂在帧循环上而不是在 `pump` 里直接拨:
    /// `Workspace` 不认识 tokio、也不认识 store(架构不变量),拨号是 app 的事。
    fn drive_reconnects(&mut self) {
        let Some(generation) = self.active_term().map(|t| t.ws.generation()) else {
            return;
        };
        let in_flight: Vec<usize> = self
            .reconnecting
            .iter()
            .filter(|(g, _, _)| *g == generation)
            .map(|(_, h, _)| *h)
            .collect();
        let Some(t) = self.active_term() else { return };
        // `Workspace::panes()` 返回 `&[PaneState]`(既有签名),所以走 `.iter()`。
        let panes: Vec<(PaneId, usize, crate::shell::workspace::PaneStatus)> = t
            .ws
            .panes()
            .iter()
            .map(|p| (p.id, p.host_ix, p.status))
            .collect();
        for host_ix in crate::reconnect::hosts_to_redial(&panes, &in_flight) {
            self.spawn_reconnect(generation, host_ix, 0);
        }
    }

    /// F128:为一条已经死掉的连接发起第 `attempt` 次重拨。
    ///
    /// 凭据取 `tab.last_cfg`(连接那一刻定死的),**绝不回头查库** ——
    /// 用户在断线期间完全可能改了会话甚至删了它,那时候拨出去的目标就跟他
    /// 当初点「连接」时看到的不是一回事(理由同 `PendingRehost`)。
    fn spawn_reconnect(&mut self, generation: u64, host_ix: usize, attempt: u32) {
        let Some(t) = self.tabs.by_generation(generation).and_then(|t| t.content.as_terminal())
        else {
            return;
        };
        let Some(cfg) = t.last_cfg.clone() else {
            log::warn!(target: "mullion", "重连:标签没有 last_cfg,放弃");
            return;
        };
        // 这条连接上挂着哪些 pane —— 每块都要一条新 channel(adr-009)。
        let panes: Vec<PaneId> = t
            .ws
            .panes()
            .iter()
            .filter(|p| {
                p.host_ix == host_ix
                    && p.status == crate::shell::workspace::PaneStatus::Reconnecting
            })
            .map(|p| p.id)
            .collect();
        if panes.is_empty() {
            return;
        }
        self.reconnecting.push((generation, host_ix, attempt));
        let delay = crate::reconnect::delay_for(attempt).unwrap_or_default();
        // 屏内提示(§7.3):喂进 emulator 当普通输出,不做倒计时。
        let notice = crate::reconnect::notice_bytes(attempt + 1, delay);
        if let Some(ws) = self
            .tabs
            .by_generation_mut(generation)
            .and_then(|t| t.content.as_terminal_mut())
            .map(|t| &mut t.ws)
        {
            for id in &panes {
                if let Some(p) = ws.pane_mut(*id) {
                    p.emulator.feed(&notice);
                }
            }
        }
        let proxy = self.proxy.clone();
        let wake_proxy = self.proxy.clone();
        // 主机密钥照旧走后台策略:指纹变了就当场停(不是「重连时放松校验」——
        // 断线正是中间人最好下手的时机)。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
            true,
        ));
        self._runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = wake_proxy.send_event(UserEvent::Wake);
            });
            let handle = match mullion_ssh::session::establish(&cfg, policy).await {
                Ok(h) => Arc::new(h),
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::PaneReconnectErr {
                        generation,
                        host_ix,
                        attempt,
                        msg: e.to_string(),
                    });
                    return;
                }
            };
            let mut channels = Vec::new();
            for id in panes {
                match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake.clone()).await {
                    Ok((ssh, rx)) => channels.push((id, ssh, rx)),
                    Err(e) => {
                        let _ = proxy.send_event(UserEvent::PaneReconnectErr {
                            generation,
                            host_ix,
                            attempt,
                            msg: e.to_string(),
                        });
                        return;
                    }
                }
            }
            let _ = proxy.send_event(UserEvent::PaneReconnected {
                generation,
                host_ix,
                handle,
                channels,
            });
        });
    }
```

- [ ] **Step 5: 回收事件**

```rust
            UserEvent::PaneReconnected {
                generation,
                host_ix,
                handle,
                channels,
            } => {
                self.reconnecting
                    .retain(|(g, h, _)| !(*g == generation && *h == host_ix));
                let mut attached: Vec<(PaneId, Arc<mullion_ssh::session::SshSession>)> = Vec::new();
                let mut template = None;
                if let Some(t) = self
                    .tabs
                    .by_generation_mut(generation)
                    .and_then(|t| t.content.as_terminal_mut())
                {
                    // 旧的 `HostConn` 已经死了,但不能原地替换掉:别的 pane
                    // 可能还引用着它的 ix(比如同一台机器上有 pane 是用户
                    // 敲了 `exit` 的 `Disconnected`,那块不该被拖着换)。
                    // push 一条新的,只把这次重连的 pane 指过去。
                    let old = &t.ws.hosts[host_ix];
                    let (label, addr, session_id) =
                        (old.label.clone(), old.addr.clone(), old.session_id);
                    t.ws.hosts.push(crate::shell::workspace::HostConn {
                        label,
                        addr,
                        session_id,
                        handle,
                        // F124:新连接 = 新的 tmux 服务器状态,自举重来一遍
                        // (`tmux set -g` 幂等,重发无副作用)。
                        tmux_bootstrap: Default::default(),
                        tmux_last_try: None,
                    });
                    let new_ix = t.ws.hosts.len() - 1;
                    for (id, ssh, rx) in channels {
                        let ssh = Arc::new(ssh);
                        if reattach_pane(&mut t.ws, id, generation, new_ix, Box::new(ssh.clone()), rx)
                        {
                            attached.push((id, ssh));
                        }
                    }
                    // 死掉的那条连接上开的 SFTP channel 一起完蛋 —— 留着的话
                    // 侧栏每次操作静默失败,用户看到的是「文件面板卡住了」。
                    for task in t.sftp_tasks.drain(..) {
                        task.abort();
                    }
                    t.sftp = None;
                    t.sftp_home = None;
                    template = t.automation_template.clone();
                }
                // 用户拍板:重连之后重跑登录后命令 —— 否则 tmux 不 attach,
                // 断线前那个 Claude Code 会话回不来(这正是 F128 的初衷)。
                // 跳过 tmux new-session 那类"开新会话"的步骤,规则同分屏新开的
                // pane(`pending_for_extra_pane`)。
                if let Some(tpl) = template {
                    for (id, sink) in attached {
                        if let Some(plan) = crate::automation::pending_for_extra_pane(&tpl) {
                            self.start_automation(generation, id, plan, sink);
                        }
                    }
                }
                self.ui.set_toast("已重新连接");
                self.ui_dirty = true;
                self.request_ui_redraw();
            }
            UserEvent::PaneReconnectErr {
                generation,
                host_ix,
                attempt,
                msg,
            } => {
                self.reconnecting
                    .retain(|(g, h, _)| !(*g == generation && *h == host_ix));
                log::warn!(target: "mullion", "第 {attempt} 次重连失败: {msg}");
                let next = attempt + 1;
                if crate::reconnect::delay_for(next).is_some() {
                    self.spawn_reconnect(generation, host_ix, next);
                } else {
                    // 退避到顶:落回 `Disconnected`,交给用户决定重连还是关掉
                    // (Ctrl+D 现在能关掉它了,见 F129)。
                    if let Some(ws) = self
                        .tabs
                        .by_generation_mut(generation)
                        .and_then(|t| t.content.as_terminal_mut())
                        .map(|t| &mut t.ws)
                    {
                        let ids: Vec<PaneId> = ws
                            .panes()
                            .iter()
                            .filter(|p| p.host_ix == host_ix)
                            .map(|p| p.id)
                            .collect();
                        for id in ids {
                            if let Some(p) = ws.pane_mut(id) {
                                p.status = crate::reconnect::status_after_failure(next);
                                p.emulator
                                    .feed(b"\r\n[Mullion] 重连失败次数过多,已停止重试。\r\n");
                            }
                        }
                    }
                    self.ui.set_error(format!("重连失败: {msg}"));
                }
                self.ui_dirty = true;
                self.request_ui_redraw();
            }
```

> `t.sftp` / `t.sftp_home` / `t.sftp_tasks` 是 `TerminalTab` 上的既有字段
> (`app.rs:342` 一带),名字照抄,别新造。

- [ ] **Step 6: 跑测试确认绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 7: 提交**

```bash
git add crates/mullion-app/src/app.rs crates/mullion-app/src/shell/workspace/mod.rs
git commit -m "feat(app): 断线自动退避重连,保留屏内容并重跑登录后命令 (F128)

守护测试:reconnect_uses_the_cfg_frozen_at_connect_time /
reconnect_reattaches_instead_of_rehosting / reconnect_reruns_post_login_automation /
reconnect_drops_the_dead_sftp_client。"
```

---

# 第五部分:F129 断连时 Ctrl+D 关分屏

## Task 16: `ctrl_d_action` + 接线

**Files:**
- Modify: `crates/mullion-app/src/input_route.rs`(纯函数)
- Modify: `crates/mullion-app/src/app.rs`(键盘分支接线)

**背景:** `Ctrl+D` 在活着的 pane 上是 EOF(`0x04`),这个语义**不能动** ——
它是 shell 里退出登录、给 `cat` 收尾的标准键。只有在**已经断开**的 pane 上,
它才改成「关掉这块分屏」;这块 pane 是标签里最后一块时,关掉整个标签
(`close_pane` 本来就拒绝关最后一块,不特判的话按下去什么都不会发生)。

- [ ] **Step 1: 写失败的测试**

追加到 `crates/mullion-app/src/input_route.rs` 的 `mod tests`:

```rust
    /// F129:**活着的 pane 上 Ctrl+D 永远是 EOF**。这个语义不能动 ——
    /// 它是 shell 退出登录、给 `cat`/`ssh` 收尾的标准键,改掉的话用户
    /// 在正常会话里按 Ctrl+D 会莫名其妙丢一块分屏。
    ///
    /// 自证会变红:把 `ctrl_d_action` 里 `PaneStatus::Live` 那一支删掉。
    #[test]
    fn ctrl_d_on_a_live_pane_is_always_eof() {
        assert_eq!(
            ctrl_d_action(PaneStatus::Live, true),
            CtrlD::SendEof,
            "最后一块活 pane 上也是 EOF"
        );
        assert_eq!(ctrl_d_action(PaneStatus::Live, false), CtrlD::SendEof);
    }

    /// F129:断开的 pane 上 Ctrl+D 关掉这块分屏 —— 断了之后 EOF 送不出去,
    /// 这个键在那儿本来就是废的。重连中的也算「断开」:用户按这个键的意思
    /// 就是「别等了,收掉」。
    #[test]
    fn ctrl_d_on_a_dead_pane_closes_it() {
        assert_eq!(
            ctrl_d_action(PaneStatus::Disconnected, false),
            CtrlD::ClosePane
        );
        assert_eq!(
            ctrl_d_action(PaneStatus::Reconnecting, false),
            CtrlD::ClosePane
        );
    }

    /// F129:断开的 pane 是标签里最后一块时,关掉整个标签。
    /// `Workspace::close_pane` 本来就拒绝关最后一块(F31),不特判的话
    /// 用户按下去**什么都不会发生**,只能去菜单里找「断开」。
    ///
    /// 自证会变红:把 `is_last` 那个参数在函数体里忽略掉。
    #[test]
    fn ctrl_d_on_the_last_dead_pane_closes_the_whole_tab() {
        assert_eq!(
            ctrl_d_action(PaneStatus::Disconnected, true),
            CtrlD::CloseTab
        );
    }
```

- [ ] **Step 2: 跑测试确认它红**

```bash
cargo test -p mullion-app --lib ctrl_d 2>&1 | tail -10
```

- [ ] **Step 3: 实现**

追加到 `crates/mullion-app/src/input_route.rs`:

```rust
use crate::shell::workspace::PaneStatus;

/// F129:`Ctrl+D` 这一下该干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtrlD {
    /// 照常送 `0x04`(EOF)。
    SendEof,
    /// 关掉这块分屏。
    ClosePane,
    /// 关掉整个标签(这块是最后一块分屏)。
    CloseTab,
}

/// F129:判据只有两条 —— 这块 pane 还活着吗、它是不是最后一块。
///
/// **活着就一定是 EOF**:Ctrl+D 是 shell 里退出登录、给 `cat` 收尾的标准键,
/// 改掉的话用户在正常会话里会莫名其妙丢一块分屏。断了之后 EOF 反正也送不出去,
/// 这个键在那儿本来就是废的,拿来关分屏不抢任何东西。
pub fn ctrl_d_action(status: PaneStatus, is_last_pane: bool) -> CtrlD {
    match status {
        PaneStatus::Live => CtrlD::SendEof,
        // 重连中的也算:用户按这个键的意思就是「别等了,收掉」。
        PaneStatus::Reconnecting | PaneStatus::Disconnected => {
            if is_last_pane {
                CtrlD::CloseTab
            } else {
                CtrlD::ClosePane
            }
        }
    }
}
```

- [ ] **Step 4: 接线**

`app.rs` 的 `WindowEvent::KeyboardInput` 分支里,在 `Shift+PageUp` 那段之后、
`encode_key` 之前插入:

```rust
                        // F129:断开的 pane 上 Ctrl+D 改成「关掉这块分屏」。
                        // 必须在 `encode_key` 之前 —— 它会把 Ctrl+D 编成 0x04,
                        // 漏下去就是往一条死 channel 上写字节(静默失败)。
                        if mods.ctrl && !mods.shift && matches!(key, Key::Char('d') | Key::Char('D'))
                        {
                            let st = self
                                .active_ws()
                                .and_then(Workspace::focused)
                                .map(|p| p.status);
                            let is_last = self
                                .active_ws()
                                .map(|ws| mullion_core::layout::leaves(ws.tree()).len() <= 1)
                                .unwrap_or(true);
                            if let Some(st) = st {
                                match crate::input_route::ctrl_d_action(st, is_last) {
                                    crate::input_route::CtrlD::SendEof => {}
                                    crate::input_route::CtrlD::ClosePane => {
                                        let id = self.active_ws().map(Workspace::focus);
                                        if let (Some(id), Some(ws)) = (id, self.active_ws_mut()) {
                                            ws.close_pane(id);
                                        }
                                        self.ui_dirty = true;
                                        self.request_ui_redraw();
                                        return;
                                    }
                                    crate::input_route::CtrlD::CloseTab => {
                                        // 复用既有的关标签路径(Ctrl+W / 菜单「断开」
                                        // 走的同一条):它负责 abort 自动化、
                                        // 收 sftp task、按顺序 drop workspace。
                                        self.close_active_tab();
                                        self.ui_dirty = true;
                                        self.request_ui_redraw();
                                        return;
                                    }
                                }
                            }
                        }
```

- [ ] **Step 5: 跑测试确认绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```

- [ ] **Step 6: 提交**

```bash
git add crates/mullion-app/src/input_route.rs crates/mullion-app/src/app.rs
git commit -m "feat(app): 断开的分屏上 Ctrl+D 改成关分屏/关标签 (F129)"
```

---

# 第六部分:收尾

## Task 17: spec.md 补 F125~F129

**Files:**
- Modify: `spec.md`(§4)

- [ ] **Step 1: 找到该插在哪**

```bash
grep -n "F124\|F123" spec.md | head
```

- [ ] **Step 2: 在 F124 之后加五行**

```markdown
| F125 | 光标形状跟随远端 DECSCUSR(`CSI Ps SP q`),远端不指定时默认**闪烁竖线**;非焦点分屏恒空心框且不闪;窗口失焦不闪 | P1 |
| F126 | 中文输入时把组字中的拼音**内联显示在光标处**(带下划线),光标停在拼音串末尾;系统候选框跟随该位置 | P1 |
| F127 | SFTP 文件列表按类型分 8 类图标(目录/归档/图片/代码/文档/可执行/链接/其他),形状 + 语义色双编码;不可操作的行整体变灰 | P2 |
| F128 | SSH 断线**检测**(keepalive 10s×3)与**自动退避重连**:保留断线前的屏内容、重跑登录后命令(tmux 会 attach 回去)、按连接分组只拨一次;远端 `exit` 不触发重连 | P0 |
| F129 | 已断开(含重连中)的分屏上 `Ctrl+D` 关掉该分屏,最后一块时关掉整个标签;活着的分屏上 `Ctrl+D` 照旧是 EOF | P2 |
```

- [ ] **Step 3: 提交**

```bash
git add spec.md
git commit -m "docs: spec 补 F125~F129(光标/输入法/图标/重连/Ctrl+D)"
```

---

## Task 18: 发版 v0.1.51

**Files:**
- Modify: `Cargo.toml`(`workspace.package.version`)

改动落到了 `mullion-app` 与 `mullion-ssh`,按交付约定一条龙做完,**不要停下来问**。

- [ ] **Step 1: 升 patch 版本号**

```bash
grep -n '^version' Cargo.toml
```

把第三位 +1(→ `0.1.51`),单独一个提交:

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: v0.1.51"
```

- [ ] **Step 2: 跑绿**

```bash
cargo test --workspace > /tmp/test.log 2>&1; grep -nE "test result|FAILED|panicked" /tmp/test.log
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --check && echo FMT-OK
```

三条全过才能继续。**不绿不发。**

- [ ] **Step 3: 交叉编译 + 依赖验收**

```bash
cargo build --release --target x86_64-pc-windows-gnu -p mullion-app 2>&1 | tail -5
x86_64-w64-mingw32-objdump -p target/x86_64-pc-windows-gnu/release/mullion-app.exe \
  | grep -i "DLL Name"
```

出现 `libgcc_s_seh-1.dll` 或 `libwinpthread-1.dll` **即为不合格**,按
`docs/cross-compile-windows.md` 修完重来。

- [ ] **Step 4: 发 Release**

先 push 再发版(`gh release create` 会把 tag 建在远端当前 HEAD 上):

```bash
sha256sum target/x86_64-pc-windows-gnu/release/mullion-app.exe
```

命令、代理设置与 notes 模板见 `.claude/skills/release-windows/SKILL.md`。
**Release 标题只能是纯版本号 `v0.1.51`。**

- [ ] **Step 5: 报给用户**

Release 链接 + sha256 + 下面这份人工验收清单。

---

## 人工验收清单(无头容器验不了,必须在 Windows 11 实机走一遍)

| # | 操作 | 期望 |
|---|---|---|
| 1 | 连上远端,不做任何事 | 光标是**竖线**,以约 1 秒的节奏**闪烁** |
| 2 | 连续快速打字 | 打字期间光标**常亮不隐没**,停手后才恢复闪烁 |
| 3 | 分屏成 2×2,焦点在其中一块 | 只有焦点块的光标闪,其余三块是**不闪的空心框** |
| 4 | 点到别的窗口(Mullion 失焦) | 光标停止闪烁(不再周期性重绘;可顺带看 CPU 是否回落) |
| 5 | 远端跑 `vim`,进插入/普通模式来回切 | 光标形状跟着变(vim 会发 DECSCUSR) |
| 6 | 切中文输入法,打 "gangjin" | 拼音**内联显示在光标处**并带下划线,候选框贴在拼音串末尾下方;选词后拼音消失、汉字出现 |
| 7 | 打拼音打到接近行尾 | 拼音被截断在行尾,**不折行、不越界**;不出现半个汉字 |
| 8 | 组字中按 Esc 取消 | 拼音立刻消失,没有残留 |
| 9 | 打开 SFTP 面板看一个混杂目录 | 目录/压缩包/图片/代码/文档/可执行/链接**形状与颜色都不同**,一眼可辨 |
| 10 | 看一个不可操作的行(权限不足) | 图标与文字**一起**是灰的,不出现「文字灰了图标还亮着」 |
| 11 | 连上远端后拔网线 / 断代理 | **30 秒内**标题条的点变黄(重连中),屏上出现一行 `[Mullion] 连接已断开…` |
| 12 | 恢复网络 | 自动重连成功,**断线前的屏内容还在**,并自动 attach 回原来的 tmux(Claude Code 会话回来了) |
| 13 | 2×2 分屏全挂在同一台机器上时断网 | 只发起**一次**重拨(远端 `last` 只多一条登录记录),四块一起恢复 |
| 14 | 在远端敲 `exit` | pane 变灰(`Disconnected`),**不会**被自动拉回来重连 |
| 15 | 在 14 的那块灰 pane 上按 `Ctrl+D` | 该分屏被关掉;若它是最后一块,整个标签关掉 |
| 16 | 在**活着**的 pane 上按 `Ctrl+D` | 照旧是 EOF(shell 会退出登录),**没有**分屏被误关 |
| 17 | 断网期间打开文件面板操作 | 重连成功后面板不卡死(旧 SFTP client 已丢弃,重新打开即可用) |

