//! VT 快照 fixture:拿**真实录下来的字节流**喂 `Emulator`,比对渲染结果。
//!
//! 录制与命名约定见 `tests/fixtures/README.md`。快照失配时先看 diff 是不是
//! 真的退化,确认是有意改动再用 `UPDATE_VT_SNAPSHOTS=1 cargo test -p mullion-term`
//! 重写 `.snap`。**不要手改 `.bin`**——要改就重录。

use std::fmt::Write as _;
use std::path::PathBuf;

use mullion_term::emulator::Emulator;
use mullion_term::palette;
use mullion_term::snapshot::{CursorShape, GridSnapshot};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// 把 `<名字>.bin` 喂进一个 `cols×rows` 的仿真器。
fn play(name: &str, cols: u16, rows: u16) -> GridSnapshot {
    let path = fixtures_dir().join(format!("{name}.bin"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()));
    let mut emu = Emulator::new(cols, rows);
    emu.feed(&bytes);
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
