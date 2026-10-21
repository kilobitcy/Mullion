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

/// F335:选区里的一个屏幕行,供 [`join_as_one_line`] 拼接。由
/// `Emulator::selection_rows` 从网格里取,这里不碰 alacritty 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelRow {
    /// 这一行被选中的文字,不含换行。
    pub text: String,
    /// 这一行最后一个非空格写到了网格**最后一列**。
    pub reaches_right_edge: bool,
    /// alacritty 记的软折行(`WRAPLINE`):程序写满一行后自动折到下一行。
    pub soft_wrapped: bool,
}

/// F335:把一段多行选区拼成**一行**(Alt+拖选)。
///
/// Claude Code(Ink)按终端宽度在词边界插**硬换行**、续行补两格缩进;一个比
/// 可用宽度还长的 token(长 URL、`a/$(b)/c` 这种)则在宽度处被硬切开。两种
/// 断行都不带 `WRAPLINE`(tmux 用绝对定位重画),只能靠网格位置区分:
///
/// - 上一行**写到了最后一列** → 被硬切开的 token,直接拼;
/// - 没写满 → 词边界断行(Ink 把行尾空格修掉了),补一个空格。
///
/// 参照必须是**网格右缘**,不能是「选区里最宽那一行」:两行选区里最宽的
/// 那行就是它自己,词边界断行会被误判成硬切、两个词粘在一起。
/// 已认下:某行在词边界处恰好写满整宽时会少一个空格。
///
/// 续行的前导空白一律去掉(Ink 的缩进),软折行的续行除外 —— 那是程序
/// 自己写满后的自动折行,行首空格是真内容。空行跳过、不影响分隔判断。
/// 整体再去首尾空白(代码块每行都有缩进)。
pub fn join_as_one_line(rows: &[SelRow]) -> String {
    let mut out = String::new();
    let mut prev: Option<&SelRow> = None;
    for row in rows {
        let verbatim = prev.is_some_and(|p| p.soft_wrapped);
        let text = row.text.trim_end();
        let text = if verbatim { text } else { text.trim_start() };
        if text.is_empty() {
            continue;
        }
        if let Some(p) = prev {
            if !(p.soft_wrapped || p.reaches_right_edge) {
                out.push(' ');
            }
        }
        out.push_str(text);
        prev = Some(row);
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str, edge: bool) -> SelRow {
        SelRow {
            text: text.into(),
            reaches_right_edge: edge,
            soft_wrapped: false,
        }
    }

    /// 词边界断行补空格、续行缩进去掉。自证会变红:删掉 `out.push(' ')`。
    #[test]
    fn a_word_boundary_break_becomes_one_space() {
        let rows = [row("  git commit -m foo", false), row("  --amend", false)];
        assert_eq!(join_as_one_line(&rows), "git commit -m foo --amend");
    }

    /// 写满整行的是被硬切开的长 token,直接拼。用户主场景:
    /// `backup@host:/srv/$(hostname)/` + `$(date +%Y%m%d)/`。
    /// 自证会变红:把判据里的 `p.reaches_right_edge` 删掉。
    #[test]
    fn a_row_that_reaches_the_right_edge_is_a_cut_token_and_joins_without_space() {
        let rows = [
            row("  backup@archive.example.com:/srv/$(hostname)/", true),
            row("  $(date +%Y%m%d)/ && echo done", false),
        ];
        assert_eq!(
            join_as_one_line(&rows),
            "backup@archive.example.com:/srv/$(hostname)/$(date +%Y%m%d)/ && echo done"
        );
    }

    /// 软折行的续行原样接上,行首空格是真内容。
    #[test]
    fn a_soft_wrapped_continuation_keeps_its_leading_space() {
        let rows = [
            SelRow {
                text: "echo a".into(),
                reaches_right_edge: true,
                soft_wrapped: true,
            },
            row(" b", false),
        ];
        assert_eq!(join_as_one_line(&rows), "echo a b");
    }

    #[test]
    fn blank_rows_are_skipped_and_the_ends_are_trimmed() {
        let rows = [
            row("   ls ", false),
            row("    ", false),
            row("  -la  ", false),
        ];
        assert_eq!(join_as_one_line(&rows), "ls -la");
        assert_eq!(join_as_one_line(&[]), "");
    }
}
