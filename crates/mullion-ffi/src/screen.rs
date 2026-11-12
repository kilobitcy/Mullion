//! 网格快照 → 纯文本行。**F345 最小终端专用**:F351 会换成「按行合并同样式段」
//! 的打包(带颜色、反显、光标形状),届时这里整体替换。

use mullion_term::snapshot::GridSnapshot;

/// 一帧屏幕的纯文本投影。
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ScreenText {
    /// 每行文本,行尾空白已去掉。宽字符只出现一次(右半占位格跳过)。
    pub rows: Vec<String>,
    pub cursor_row: u16,
    /// 光标所在的**格**列号(宽字符占两格),不是 `rows[r]` 里的字符下标。
    pub cursor_col: u16,
}

pub fn project(snap: &GridSnapshot) -> ScreenText {
    let rows = (0..snap.rows)
        .map(|r| {
            let line: String = snap
                .row(r)
                .iter()
                .filter(|c| !c.spacer)
                .map(|c| c.ch)
                .collect();
            line.trim_end().to_string()
        })
        .collect();
    ScreenText {
        rows,
        cursor_row: snap.cursor.row,
        cursor_col: snap.cursor.col,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_term::emulator::Emulator;

    fn screen_after(bytes: &[u8]) -> ScreenText {
        let mut e = Emulator::new(10, 3);
        e.feed(bytes);
        project(&e.snapshot())
    }

    #[test]
    fn plain_rows_come_out_trimmed() {
        let s = screen_after(b"ab\r\ncd");
        assert_eq!(s.rows, vec!["ab", "cd", ""]);
    }

    #[test]
    fn a_wide_char_appears_once_not_twice() {
        // 宽字符右半是 spacer 格(F16),不跳过就会变成「中中」。
        let s = screen_after("a中b".as_bytes());
        assert_eq!(s.rows[0], "a中b");
    }

    #[test]
    fn cursor_is_reported_in_cells_not_chars() {
        let s = screen_after("中".as_bytes());
        assert_eq!((s.cursor_row, s.cursor_col), (0, 2));
    }
}
