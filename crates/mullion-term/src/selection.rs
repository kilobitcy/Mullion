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

/// F371:算不算路径里的字符。只认 ASCII —— 中文文件名的路径会在中文处截断
/// (设计已认下,退回拖选);换来的是 `见docs/x.md`、`设计文档：docs/…` 这种
/// 中文紧贴路径的写法能切干净。
pub fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "._-/~+@%=".contains(c)
}

/// F371:双击落在 `(row, col)` 上,若那里是一条路径,返回它的起止格(含两端,
/// `(行, 列)`,可跨行);不是路径返回 `None`,交回 alacritty 语义选择。
///
/// - 向两侧扩到路径字符的最大连续段,该段**含 `/`** 才算路径;段尾的 `.` 不剥
///   (与 app 侧 `files::reveal::TRAILING_PUNCT` 对齐)。
/// - 跨行用 F335 同一条「写满右缘」判据(见 [`join_as_one_line`]):tmux 绝对
///   定位重画,断行不带 `WRAPLINE`。段写到最后一列、下一行第 0 列也是路径字符
///   → 同一个 token 被硬切开了,续上;向上同理。恰好写满又恰好下一行以路径
///   字符开头而两者无关时会多选一截,已认下。
///
/// `rows` 每行按**列**排(宽字 spacer 由调用方换成非路径字符),行宽一致。
pub fn path_span(
    rows: &[Vec<char>],
    row: usize,
    col: usize,
) -> Option<((usize, usize), (usize, usize))> {
    let at = |r: usize, c: usize| rows.get(r).and_then(|l| l.get(c)).copied();
    if !at(row, col).is_some_and(is_path_char) {
        return None;
    }
    let last = |r: usize| rows[r].len().saturating_sub(1);
    let (mut sr, mut sc) = (row, col);
    loop {
        while sc > 0 && at(sr, sc - 1).is_some_and(is_path_char) {
            sc -= 1;
        }
        if sc == 0 && sr > 0 && at(sr - 1, last(sr - 1)).is_some_and(is_path_char) {
            sr -= 1;
            sc = last(sr);
            continue;
        }
        break;
    }
    let (mut er, mut ec) = (row, col);
    loop {
        while ec < last(er) && at(er, ec + 1).is_some_and(is_path_char) {
            ec += 1;
        }
        if ec == last(er) && at(er + 1, 0).is_some_and(is_path_char) {
            er += 1;
            ec = 0;
            continue;
        }
        break;
    }
    let has_slash = rows[sr..=er].iter().enumerate().any(|(i, line)| {
        let from = if i == 0 { sc } else { 0 };
        let to = if sr + i == er { ec } else { line.len() - 1 };
        line[from..=to].contains(&'/')
    });
    has_slash.then_some(((sr, sc), (er, ec)))
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

    /// 网格:每行补空格到 `width` 列。中文按一格算 —— 宽字的 spacer 由
    /// `Emulator` 换成空格,对路径判据而言两者都只是「非路径字符」。
    fn grid(width: usize, lines: &[&str]) -> Vec<Vec<char>> {
        lines
            .iter()
            .map(|l| {
                let mut v: Vec<char> = l.chars().collect();
                v.resize(width, ' ');
                v
            })
            .collect()
    }

    /// F371:中文冒号后紧跟的路径,点在路径里任意一格都选出整段,且只有那一段。
    #[test]
    fn a_double_click_anywhere_in_a_path_takes_the_whole_path() {
        let g = grid(60, &["设计文档：docs/superpowers/specs/x-design.md 之后"]);
        // `设计文档：` 占 5 个字符格,路径从第 5 列起。
        let len = "docs/superpowers/specs/x-design.md".len();
        for col in [5, 9, 20, 5 + len - 1] {
            assert_eq!(
                path_span(&g, 0, col),
                Some(((0, 5), (0, 5 + len - 1))),
                "点在第 {col} 列"
            );
        }
    }

    /// 不含 `/` 的词不算路径,交回语义选择。
    #[test]
    fn a_word_without_a_slash_is_not_a_path() {
        let g = grid(40, &["hello world.txt"]);
        assert_eq!(path_span(&g, 0, 2), None);
        assert_eq!(path_span(&g, 0, 8), None);
    }

    /// 点在非路径字符上(空格、中文、全角冒号)不走路径判据。
    #[test]
    fn a_click_off_the_path_is_not_a_path() {
        let g = grid(40, &["见：docs/a.md"]);
        assert_eq!(path_span(&g, 0, 0), None);
        assert_eq!(path_span(&g, 0, 1), None);
    }

    /// 段尾的 `.` 不剥(与 `files::reveal::TRAILING_PUNCT` 对齐);`,` `:`
    /// 本就不是路径字符。
    #[test]
    fn a_trailing_dot_is_kept_and_a_comma_ends_the_path() {
        let g = grid(40, &["at ../x/y. and a/b, c"]);
        assert_eq!(path_span(&g, 0, 4), Some(((0, 3), (0, 9))));
        assert_eq!(path_span(&g, 0, 16), Some(((0, 15), (0, 17))));
    }

    /// 写满右缘、下一行行首紧接路径字符 → 被硬切开的同一条路径,跨行续上。
    /// 两头都点一次:从上半截往下续、从下半截往上续,结果相同。
    #[test]
    fn a_path_cut_at_the_right_edge_continues_on_the_next_row() {
        let g = grid(10, &["see a/bcde", "f/gh.rs ok"]);
        let want = Some(((0, 4), (1, 6)));
        assert_eq!(path_span(&g, 0, 6), want);
        assert_eq!(path_span(&g, 1, 2), want);
    }

    /// 没写满就不续:那是词边界断行,下一行是另一段文字。
    #[test]
    fn a_path_that_stops_short_of_the_edge_does_not_continue() {
        let g = grid(12, &["see a/bcde", "fgh/x.rs"]);
        assert_eq!(path_span(&g, 0, 6), Some(((0, 4), (0, 9))));
        assert_eq!(path_span(&g, 1, 1), Some(((1, 0), (1, 7))));
    }

    /// 写满了但下一行行首是空格(Ink 续行缩进)→ 不续。
    #[test]
    fn a_full_row_followed_by_an_indented_row_does_not_continue() {
        let g = grid(10, &["see a/bcde", "  fgh"]);
        assert_eq!(path_span(&g, 0, 6), Some(((0, 4), (0, 9))));
    }
}
