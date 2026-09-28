//! F299:内置编辑器的查找逻辑。零 egui,纯函数。
//!
//! 位置一律是**字符下标**(不是字节):egui 的 `CCursor` 按字符计,
//! 给字节下标的话中文文件里第一处匹配之后全部错位。

/// 在 `text` 里找 `query` 的全部非重叠出现,返回 `[起, 止)` 字符区间。
/// `query` 空 = 没有匹配。`case` 为假时逐字符小写折叠后比较。
pub fn find_all(text: &str, query: &str, case: bool) -> Vec<(usize, usize)> {
    if query.is_empty() {
        return Vec::new();
    }
    let fold = |c: char| -> char {
        if case {
            c
        } else {
            // 只取小写映射的第一个字符:个别字符(如 'İ')小写后是两个字符,
            // 展开会让字符下标和原文对不上。
            c.to_lowercase().next().unwrap_or(c)
        }
    };
    let hay: Vec<char> = text.chars().map(fold).collect();
    let needle: Vec<char> = query.chars().map(fold).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            out.push((i, i + needle.len()));
            i += needle.len();
        } else {
            i += 1;
        }
    }
    out
}

/// 从光标 `at`(字符下标)出发第一处 `起点 >= at` 的匹配;没有就回绕到第一处。
pub fn first_at_or_after(hits: &[(usize, usize)], at: usize) -> Option<usize> {
    if hits.is_empty() {
        return None;
    }
    Some(hits.iter().position(|h| h.0 >= at).unwrap_or(0))
}

/// 下一个 / 上一个,首尾回绕。
pub fn step(len: usize, cur: usize, forward: bool) -> usize {
    if len == 0 {
        return 0;
    }
    if forward {
        (cur + 1) % len
    } else {
        (cur + len - 1) % len
    }
}

/// `3/17` 这样的计数;没有匹配时 `0/0`。
pub fn counter(cur: Option<usize>, len: usize) -> String {
    match cur {
        Some(i) if len > 0 => format!("{}/{}", i + 1, len),
        _ => format!("0/{len}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 字符下标而非字节:中文在前,匹配位置要按字符算。
    /// 自证会变红:把 `hay` 改成按字节(`text.bytes()`)建。
    #[test]
    fn positions_are_char_indices_even_after_cjk() {
        assert_eq!(find_all("中文abc", "abc", true), vec![(2, 5)]);
    }

    /// 默认不区分大小写;区分时只命中同形。
    /// 自证会变红:让 `fold` 恒返回原字符。
    #[test]
    fn case_folding_is_off_by_default_and_on_when_asked() {
        assert_eq!(find_all("Foo foo FOO", "foo", false).len(), 3);
        assert_eq!(find_all("Foo foo FOO", "foo", true), vec![(4, 7)]);
    }

    #[test]
    fn matches_do_not_overlap_and_empty_query_matches_nothing() {
        assert_eq!(find_all("aaaa", "aa", true), vec![(0, 2), (2, 4)]);
        assert!(find_all("abc", "", false).is_empty());
    }

    /// 首尾回绕。自证会变红:去掉 `% len`。
    #[test]
    fn stepping_wraps_both_ways() {
        assert_eq!(step(3, 2, true), 0);
        assert_eq!(step(3, 0, false), 2);
        assert_eq!(step(0, 0, true), 0);
    }

    #[test]
    fn first_hit_from_the_caret_wraps_to_the_top() {
        let hits = [(1, 2), (5, 6)];
        assert_eq!(first_at_or_after(&hits, 0), Some(0));
        assert_eq!(first_at_or_after(&hits, 3), Some(1));
        assert_eq!(
            first_at_or_after(&hits, 9),
            Some(0),
            "越过最后一处要回到第一处"
        );
        assert_eq!(first_at_or_after(&[], 0), None);
    }

    #[test]
    fn the_counter_reads_one_based() {
        assert_eq!(counter(Some(2), 17), "3/17");
        assert_eq!(counter(None, 0), "0/0");
    }
}
