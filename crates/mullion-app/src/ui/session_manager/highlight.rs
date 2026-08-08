//! 搜索命中片段的切分(走查 22)。**纯函数,零 egui。**
//!
//! 搜到十几行之后,用户还得逐行自己找「到底哪儿匹配上了」。把命中的那几个
//! 字染个色就够了 —— 这个判据得能单测,所以切分和着色分开:这里只回答
//! 「哪几段是命中的」,着色留给 `list.rs`。

/// 把 `text` 按 `query` 的命中位置切成若干段,`true` = 这一段是命中的。
///
/// 空查询(或 trim 后为空)返回整段 `false` —— 调用方不用特判。
///
/// **大小写不敏感**,与 `list::matches` 的判据一致:那边放行了这一行,这边
/// 却标不出命中在哪,用户会以为搜索坏了。
///
/// 全程在 `char` 上比对、按 `char` 切,不碰字节下标 —— 中文会话名按字节切
/// 会当场 panic,而中文名在这个项目里是常态。折叠大小写只取
/// `to_lowercase()` 的**第一个** char:少数字符(如 'İ')小写成两个 char,
/// 逐 char 一一对应就没了。取首个 char 会让这类字符的匹配退化成近似,
/// 但绝不会错位或 panic —— 高亮标偏一个字远比崩掉可接受。
pub(super) fn segments(text: &str, query: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let hay: Vec<char> = chars.iter().map(|c| fold(*c)).collect();
    let needle: Vec<char> = query.trim().chars().map(fold).collect();

    if needle.is_empty() || needle.len() > hay.len() {
        return vec![(text.to_string(), false)];
    }

    let mut out: Vec<(String, bool)> = Vec::new();
    let mut i = 0;
    let mut plain_start = 0;
    while i + needle.len() <= hay.len() {
        if hay[i..i + needle.len()] == needle[..] {
            if plain_start < i {
                out.push((chars[plain_start..i].iter().collect(), false));
            }
            out.push((chars[i..i + needle.len()].iter().collect(), true));
            i += needle.len();
            plain_start = i;
        } else {
            i += 1;
        }
    }
    if plain_start < chars.len() {
        out.push((chars[plain_start..].iter().collect(), false));
    }
    if out.is_empty() {
        out.push((text.to_string(), false));
    }
    out
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(text: &str, q: &str) -> Vec<(String, bool)> {
        segments(text, q)
    }

    #[test]
    fn an_empty_query_leaves_the_text_in_one_unhighlighted_piece() {
        assert_eq!(seg("web01", ""), vec![("web01".to_string(), false)]);
        assert_eq!(seg("web01", "   "), vec![("web01".to_string(), false)]);
    }

    #[test]
    fn every_occurrence_is_marked_and_the_pieces_rejoin_to_the_original() {
        let got = seg("web01-web02", "web");
        assert_eq!(
            got,
            vec![
                ("web".to_string(), true),
                ("01-".to_string(), false),
                ("web".to_string(), true),
                ("02".to_string(), false),
            ]
        );
        // 不变量:拼回去必须一字不差 —— 高亮切分不许吃字或加字。
        let joined: String = got.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "web01-web02");
    }

    /// 大小写不敏感,且**保留原始拼写**:搜 `WEB` 时高亮的那段还得显示成
    /// `web`,不能把用户的会话名改成大写画出来。
    #[test]
    fn matching_ignores_case_but_the_original_spelling_is_kept() {
        assert_eq!(
            seg("Web01", "wEb"),
            vec![("Web".to_string(), true), ("01".to_string(), false)]
        );
    }

    /// 中文名按**字符**切。按字节切会当场 panic,而中文会话名在这个项目里
    /// 是常态。
    #[test]
    fn a_multibyte_name_is_split_on_char_boundaries() {
        let got = seg("生产环境 web01", "环境");
        assert_eq!(got[0], ("生产".to_string(), false));
        assert_eq!(got[1], ("环境".to_string(), true));
        let joined: String = got.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, "生产环境 web01");
    }

    #[test]
    fn a_query_longer_than_the_text_matches_nothing() {
        assert_eq!(seg("ab", "abcd"), vec![("ab".to_string(), false)]);
        assert_eq!(seg("", "a"), vec![(String::new(), false)]);
    }
}
