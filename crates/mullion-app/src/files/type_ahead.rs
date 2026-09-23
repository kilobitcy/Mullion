//! F292:文件面板「按字母定位」的纯判据。零 IO、零 egui;`now` 注入,
//! 与 `input::click_kind` 同一套写法。
//!
//! 行为照 Windows 资源管理器:按一个字符跳到**下一个**以它开头的条目(从
//! 光标之后找起,到底绕回);1 秒内连按累积成前缀;同一字母反复按 = 在同
//! 字母条目间循环。目录/文件一视同仁,按当前显示顺序找。

use std::time::Instant;

use mullion_ssh::sftp::{Entry, RemotePath};

/// 两次按键相隔不超过这个毫秒数就累积成前缀。
pub const TYPE_AHEAD_MS: u128 = 1000;

/// 上一次按键留下的状态。存在 `PaneState` 上(两栏各一份)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAhead {
    pub prefix: String,
    pub at: Instant,
}

/// winit 的 `Key::Character(s)` 里哪些算「可打印字符」:恰好一个 `char`,
/// 不是空白也不是控制字符。空格**不算**(留给将来的选中切换)。
pub fn key_char(s: &str) -> Option<char> {
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() || c.is_whitespace() || c.is_control() {
        return None;
    }
    Some(c)
}

/// 下一个该落到哪一行,以及更新后的状态。`rows` 是这一帧**画出来**的行
/// (`PaneState::rows()`,已过隐藏项过滤 + 排序),`cursor` 是当前光标行。
///
/// 起点规则:单字符(含循环模式)从光标**之后**找;多字符前缀从光标**本身**
/// 找起 —— 用户敲 `d`、`o` 时光标已经在 `d...` 上,再跳过它就把 `do...`
/// 本身漏了。找不到返回 `None`,状态照样更新(下一键仍按累积算)。
pub fn next_index(
    rows: &[&Entry],
    cursor: Option<&RemotePath>,
    prev: Option<&TypeAhead>,
    now: Instant,
    ch: char,
) -> (Option<usize>, TypeAhead) {
    let prefix = match prev {
        Some(p) if now.duration_since(p.at).as_millis() <= TYPE_AHEAD_MS => {
            let mut s = p.prefix.clone();
            s.push(ch);
            s
        }
        _ => ch.to_string(),
    };
    let lower: String = prefix.chars().flat_map(char::to_lowercase).collect();
    let first = lower.chars().next().unwrap_or(ch);
    let cycling = lower.chars().count() > 1 && lower.chars().all(|c| c == first);
    let needle: String = if cycling { first.to_string() } else { lower };
    let state = TypeAhead { prefix, at: now };
    if rows.is_empty() {
        return (None, state);
    }
    let at = cursor.and_then(|c| rows.iter().position(|e| e.name == *c));
    let single = cycling || state.prefix.chars().count() == 1;
    let start = match at {
        Some(i) if single => i + 1,
        Some(i) => i,
        None => 0,
    };
    let n = rows.len();
    let hit = (0..n).map(|k| (start + k) % n).find(|&ix| {
        let name: String = rows[ix]
            .name
            .display()
            .chars()
            .flat_map(char::to_lowercase)
            .collect();
        name.starts_with(&needle)
    });
    (hit, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_ssh::sftp::EntryKind;
    use std::time::Duration;

    fn entry(name: &str) -> Entry {
        Entry {
            name: RemotePath::from_bytes(name.as_bytes().to_vec()),
            kind: EntryKind::File,
            size: 1024,
            mtime: 1_700_000_000,
            mode: 0o644,
            uid: 1000,
            gid: 1000,
            link_target: None,
        }
    }

    /// 单字符从光标**之后**找,到底绕回;大小写不敏感。
    ///
    /// 自证会变红:把 `single` 那条 `i + 1` 改成 `i`(光标在 `apple` 上按 a
    /// 会原地不动);或把小写化删掉(`B` 找不到 `banana`)。
    #[test]
    fn a_single_letter_jumps_to_the_next_match_after_the_cursor_and_wraps() {
        let rows_owned = [entry("apple"), entry("banana"), entry("avocado")];
        let rows: Vec<&Entry> = rows_owned.iter().collect();
        let t0 = Instant::now();
        let (hit, st) = next_index(&rows, Some(&rows[0].name), None, t0, 'a');
        assert_eq!(hit, Some(2), "从 apple 之后找,落到 avocado");
        let (hit, _) = next_index(
            &rows,
            Some(&rows[2].name),
            Some(&st),
            t0 + Duration::from_millis(5000),
            'a',
        );
        assert_eq!(hit, Some(0), "超时后是新的单字符,绕回 apple");
        let (hit, _) = next_index(&rows, None, None, t0, 'B');
        assert_eq!(hit, Some(1), "没有光标从头找;大写也认");
        let (hit, _) = next_index(&rows, None, None, t0, 'z');
        assert_eq!(hit, None);
    }

    /// 1 秒内连按累积成前缀,从光标**本身**找起(光标已在 `d...` 上时不跳过它)。
    ///
    /// 自证会变红:把多字符那条 `Some(i) => i` 改成 `i + 1`(`do` 会跳过
    /// `dog` 落到 `door`);或把 `TYPE_AHEAD_MS` 改成 0。
    #[test]
    fn letters_within_a_second_accumulate_into_a_prefix() {
        let rows_owned = [entry("cat"), entry("dog"), entry("door"), entry("dot")];
        let rows: Vec<&Entry> = rows_owned.iter().collect();
        let t0 = Instant::now();
        let (hit, st) = next_index(&rows, None, None, t0, 'd');
        assert_eq!(hit, Some(1));
        let (hit, st) = next_index(
            &rows,
            Some(&rows[1].name),
            Some(&st),
            t0 + Duration::from_millis(300),
            'o',
        );
        assert_eq!(hit, Some(1), "`do` 仍然是 dog 自己");
        assert_eq!(st.prefix, "do");
        let (hit, st) = next_index(
            &rows,
            Some(&rows[1].name),
            Some(&st),
            t0 + Duration::from_millis(600),
            'o',
        );
        assert_eq!(hit, Some(2), "`doo` → door");
        assert_eq!(st.prefix, "doo");
        let (hit, _) = next_index(
            &rows,
            Some(&rows[2].name),
            Some(&st),
            t0 + Duration::from_millis(900),
            'x',
        );
        assert_eq!(hit, None, "`doox` 没有,光标不动");
    }

    /// 同一字母反复按 = 在同字母条目间循环(`dd` 不是找 `dd` 开头,是第二个 d)。
    ///
    /// 自证会变红:删掉 `cycling` 判据(`dd` 会去找以 `dd` 开头的,返回 None)。
    #[test]
    fn repeating_the_same_letter_cycles_through_that_letter() {
        let rows_owned = [entry("dog"), entry("door"), entry("dot"), entry("egg")];
        let rows: Vec<&Entry> = rows_owned.iter().collect();
        let t0 = Instant::now();
        let (hit, st) = next_index(&rows, None, None, t0, 'd');
        assert_eq!(hit, Some(0));
        let (hit, st) = next_index(
            &rows,
            Some(&rows[0].name),
            Some(&st),
            t0 + Duration::from_millis(200),
            'D',
        );
        assert_eq!(hit, Some(1), "大小写混按也算循环");
        let (hit, st) = next_index(
            &rows,
            Some(&rows[1].name),
            Some(&st),
            t0 + Duration::from_millis(400),
            'd',
        );
        assert_eq!(hit, Some(2));
        let (hit, _) = next_index(
            &rows,
            Some(&rows[2].name),
            Some(&st),
            t0 + Duration::from_millis(600),
            'd',
        );
        assert_eq!(hit, Some(0), "绕回");
    }

    /// 空列表没有答案但状态照样记。
    #[test]
    fn an_empty_list_has_no_answer() {
        let (hit, st) = next_index(&[], None, None, Instant::now(), 'a');
        assert_eq!(hit, None);
        assert_eq!(st.prefix, "a");
    }

    /// `key_char`:单个可打印字符才算;空格、制表、多字符串、控制字符都不算。
    ///
    /// 自证会变红:删掉 `is_whitespace()` 那一项(空格会开始定位)。
    #[test]
    fn only_a_single_printable_character_counts() {
        assert_eq!(key_char("a"), Some('a'));
        assert_eq!(key_char("中"), Some('中'));
        assert_eq!(key_char(" "), None);
        assert_eq!(key_char("\t"), None);
        assert_eq!(key_char("ab"), None);
        assert_eq!(key_char(""), None);
        assert_eq!(key_char("\u{1b}"), None);
    }
}
