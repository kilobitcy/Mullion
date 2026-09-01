//! VT 快照 fixture:拿**真实录下来的字节流**喂 `Emulator`,比对渲染结果。
//!
//! 录制与命名约定见 `tests/fixtures/README.md`。快照失配时先看 diff 是不是
//! 真的退化,确认是有意改动再用 `UPDATE_VT_SNAPSHOTS=1 cargo test -p mullion-term`
//! 重写 `.snap`。**不要手改 `.bin`**——要改就重录。

use std::fmt::Write as _;
use std::path::PathBuf;

use mullion_term::emulator::Emulator;
use mullion_term::palette;
use mullion_term::selection::{CellSide, SelectionKind};
use mullion_term::snapshot::{CursorShape, GridSnapshot};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(format!("{name}.bin"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()))
}

/// 把 `<名字>.bin` 喂进一个 `cols×rows` 的仿真器。
fn play(name: &str, cols: u16, rows: u16) -> GridSnapshot {
    let mut emu = Emulator::new(cols, rows);
    emu.feed(&fixture_bytes(name));
    emu.snapshot()
}

/// 反显格(SGR 7)在快照里的判据:前景/背景**对调过**。
///
/// 直接比 `bg == DEFAULT_FG` 会把「程序显式把底色设成前景色」也算进来,
/// 那不是同一件事;两边都要对上才算。
fn is_inverted(cell: &mullion_term::snapshot::SnapCell) -> bool {
    cell.bg == palette::DEFAULT_FG && cell.fg == palette::DEFAULT_BG
}

/// 渲染成「纯文本网格 + 属性摘要」。**属性摘要在前**:快照失配时先看到的
/// 应该是光标与反显这些语义位,而不是去一百列宽的文本里数格子。
fn render(snap: &GridSnapshot) -> String {
    let mut out = String::new();
    let c = &snap.cursor;
    let _ = writeln!(
        out,
        "cursor row={} col={} visible={} shape={:?} blinking={}",
        c.row, c.col, c.visible, c.shape, c.blinking
    );
    let inverted: Vec<String> = (0..snap.rows)
        .flat_map(|r| {
            snap.row(r)
                .iter()
                .enumerate()
                .filter(|(_, cell)| is_inverted(cell))
                .map(move |(col, _)| format!("{r},{col}"))
                .collect::<Vec<_>>()
        })
        .collect();
    let _ = writeln!(out, "inverse cells: [{}]", inverted.join(" "));
    out.push_str("--- grid ---\n");
    for r in 0..snap.rows {
        let mut line = String::new();
        for cell in snap.row(r) {
            if cell.spacer {
                continue; // 宽字符右半:字形由左格承载
            }
            line.push(if cell.ch == '\0' { ' ' } else { cell.ch });
        }
        let _ = writeln!(out, "{:02}|{}", r, line.trim_end());
    }
    out
}

/// 与 `<名字>.snap` 比对。`UPDATE_VT_SNAPSHOTS=1` 时重写。
fn assert_snapshot(name: &str, actual: &str) {
    let path = fixtures_dir().join(format!("{name}.snap"));
    if std::env::var_os("UPDATE_VT_SNAPSHOTS").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "读不到 {}: {e}(首次生成用 UPDATE_VT_SNAPSHOTS=1)",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "\n{name} 的渲染结果和快照对不上。确认是有意改动后用 \
         `UPDATE_VT_SNAPSHOTS=1 cargo test -p mullion-term` 重写"
    );
}

/// F198 / F197:**Claude Code 的输入框光标是一格 SGR 7 反显块,真光标全程隐藏。**
///
/// 字节流录自 `tmux new-session -x 100 -y 30 claude`(pipe-pane 抓 pane 输出,
/// 已等长脱敏),最后一帧是输入了 `hi` 之后停在输入框等待的稳定态。
///
/// 这条 fixture 钉的是两件在真机上一起出现、单独看都会得出错误结论的事实:
///
/// 1. 流里 `?25h` 只在启动时出现一次,随后 `?25l` 一直没再撤 —— 所以我们照画
///    真光标就是错的(F197),它停在最后写字那一格。
/// 2. 输入位置全靠 `❯ ` 后面那一格 `\e[7m \e[27m` 表示。不认 SGR 7 的话它退化
///    成普通空格,**输入框里连一个光标指示都不剩**(F198 的实报症状)。
///
/// 自证会变红:把 `Emulator::snapshot` 里 `Flags::INVERSE` 那段删掉——
/// `inverse cells` 立刻空掉。
#[test]
fn claude_code_draws_its_input_cursor_with_an_inverse_cell_while_the_real_one_stays_hidden() {
    let snap = play("claude-code-input-cursor", 100, 30);

    assert_eq!(
        snap.cursor.shape,
        CursorShape::Hidden,
        "Claude Code 自绘光标期间常驻 `?25l`,快照该报 Hidden"
    );

    let inverted: Vec<(u16, usize)> = (0..snap.rows)
        .flat_map(|r| {
            snap.row(r)
                .iter()
                .enumerate()
                .filter(|(_, cell)| is_inverted(cell))
                .map(move |(col, _)| (r, col))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        inverted.len(),
        1,
        "输入框里应当恰好有一格反显块当光标,实得 {inverted:?}"
    );

    assert_snapshot("claude-code-input-cursor", &render(&snap));
}

/// `/compact` 期间的重绘流。用来钉 F212。
const COMPACT: &str = "claude-code-compact-repaint";
const COMPACT_COLS: u16 = 120;
const COMPACT_ROWS: u16 = 30;
/// 流里那一帧「任务转完、把转圈行擦掉」的同步块的起点。
/// 它内含唯一一处会命中选区的 `CSI K`(在第 22 行)。
const ERASING_BLOCK: &[u8] = b"\x1b[?2026h\x1b[H\r\x1b[16B";

/// 把 fixture 切成「擦行之前 / 擦行及其之后」两段。
fn compact_split() -> (Vec<u8>, Vec<u8>) {
    let bytes = fixture_bytes(COMPACT);
    let at = bytes
        .windows(ERASING_BLOCK.len())
        .position(|w| w == ERASING_BLOCK)
        .expect("fixture 里应当有那个擦行的同步块");
    (bytes[..at].to_vec(), bytes[at..].to_vec())
}

/// 在第 18~24 行上拉一段跨行选区(转圈行 22 落在中间)。
fn drag_across_the_spinner(emu: &mut Emulator) -> String {
    emu.selection_start(2, 18, SelectionKind::Simple, CellSide::Left);
    emu.selection_update(20, 24, CellSide::Right);
    emu.selection_text().expect("这几行上应当有可选的文本")
}

/// F212:**擦一行会把整段跨行选区全丢掉,连没被碰过的行一起。**
///
/// 字节流录自 `tmux -x 120 -y 30` 里跑 Claude Code 执行 `/compact`(pipe-pane 抓
/// pane 输出)。流里唯一一处 `CSI K` 落在第 22 行——那是转圈提示行,任务转完就
/// 擦掉。而用户此刻按着左键从第 18 行拖到第 24 行想复制上面的输出。
///
/// alacritty 的 `clear_line` 判据是 `!s.intersects_range(擦掉的那几格)`:**沾边
/// 就整段丢**。于是 18~21 行那些根本没被碰过的高亮也一起没了,用户看到的就是
/// 「高亮出现又被冲掉」。更要命的是 [`Emulator::selection_update`] 在 `None` 上是
/// 静默 no-op —— 拖到天涯海角也回不来,只能松手重按;而 `/compact` 期间它每秒
/// 擦好几次,等于**整个划选功能在这段时间里不可用**。
///
/// 自证会变红:把 `Emulator::feed` 里那段补回逻辑删掉,下半场立刻退化成上半场。
#[test]
fn a_repaint_that_erases_one_line_must_not_take_the_whole_held_selection_with_it() {
    let (before, after) = compact_split();

    // 上半场:不按住 —— 这是 alacritty 的原生行为,钉住它才知道补偿在补什么。
    let mut loose = Emulator::new(COMPACT_COLS, COMPACT_ROWS);
    loose.feed(&before);
    let wanted = drag_across_the_spinner(&mut loose);
    loose.feed(&after);
    assert_eq!(
        loose.selection_text(),
        None,
        "上游 alacritty 的行为变了(不再因擦行丢选区),F212 的补偿要重新评估"
    );

    // 下半场:按住左键 —— 选区是本地意图,远端擦行无权取消。
    let mut held = Emulator::new(COMPACT_COLS, COMPACT_ROWS);
    held.feed(&before);
    let same = drag_across_the_spinner(&mut held);
    assert_eq!(same, wanted, "两场的起点必须一致,否则下面比的不是同一件事");
    held.hold_selection(true);
    held.feed(&after);
    let survived = held
        .selection_text()
        .expect("按住左键时,远端擦掉一行不该把整段选区冲掉");

    // 第 22 行确实被擦空了,所以文本不会与擦行前逐字相同;但没被碰过的
    // 第 18 行必须原样还在——这正是用户丢掉的那部分。
    let first_line = wanted.lines().next().unwrap();
    assert!(
        survived.contains(first_line),
        "没被擦到的行也丢了。期望仍含 {first_line:?},实得 {survived:?}"
    );
}

/// F212 的边界:松手之后远端再擦行,选区就该正常消失 —— 补偿只在按住期间生效,
/// 不是「选区从此永生」。挂住的 hold 会让这个 pane 的选区再也擦不掉。
#[test]
fn once_the_button_is_up_an_erase_clears_the_selection_again() {
    let (before, after) = compact_split();
    let mut emu = Emulator::new(COMPACT_COLS, COMPACT_ROWS);
    emu.feed(&before);
    drag_across_the_spinner(&mut emu);
    emu.hold_selection(true);
    emu.hold_selection(false);
    emu.feed(&after);
    assert_eq!(emu.selection_text(), None, "松手后不该再兜底");
}
