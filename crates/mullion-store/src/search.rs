//! F245:列表搜索的**分词**与命中判据。纯函数,零 UI、零 IO。
//!
//! 两处匹配(会话管理器的 `ui::session_manager::list::matches`、三处项目列表
//! 共用的 `crate::project::matches`)和一处高亮切分
//! (`ui::session_manager::highlight::segments`)共用这一份分词。
//!
//! **为什么必须共用**:高亮和匹配是同一件事的两半 —— 匹配放行了这一行,高亮
//! 负责说明「凭哪几个字放行的」。各切各的话,同一个查询在两处切出不同的词,
//! 症状是「搜得到,但一个字都不高亮」,而且全程不报错。`highlight` 模块自己的
//! 文档里已经为大小写这一维写过同样的话。

/// 把查询切成词。**空白分隔**,连续空白算一个分隔符,首尾空白丢掉。
///
/// 走 `split_whitespace()` 而不是 `split(' ')`:前者按 Unicode `White_Space`
/// 属性切,**包含中文输入法打出的全角空格 U+3000**。用 `' '` 的话,用户在中文
/// 输入状态下敲的空格会变成词的一部分(「爬虫　219」切不开),搜索静默失效 ——
/// 而 Windows 11 是这个项目唯一的一等公民,中文输入法是常态。
pub fn tokens(query: &str) -> Vec<&str> {
    query.split_whitespace().collect()
}

/// 这个词在这些字段里的**任意一个**出现了吗。大小写不敏感。
///
/// `token` 由调用方保证已经折叠成小写(走 [`tokens`] 之后 `to_lowercase()`),
/// `fields` 在这里折叠 —— 字段多、词少,反过来会多折叠很多次。
fn token_hits_lowered(token: &str, lowered: &[String]) -> bool {
    lowered.iter().any(|f| f.contains(token))
}

/// 这个词在这些字段里出现了吗。两边都在这里折叠成小写。
pub fn token_hits(token: &str, fields: &[&str]) -> bool {
    let lowered: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    token_hits_lowered(&token.to_lowercase(), &lowered)
}

/// 每个词都得在**某个**字段里出现 —— 词之间 AND、字段之间 OR,大小写不敏感。
///
/// 空查询(切出来一个词都没有)放行全部,调用方不用特判。
///
/// **不是子序列匹配(fzf 式)**:项目的「说明」是多行长文本(F237),子序列在
/// 上面几乎必然命中任意短查询 —— 随便打三个字符都能按顺序找出来,结果是
/// 「一搜就全中」,搜索表面上有反应、实际失效。
///
/// **词之间是 AND 不是 OR**:用户记得住的是「那个跑爬虫的、在 219 那台」,
/// 输入 `爬虫 219` 要的是同时满足;OR 会把只沾一个词的行全捞出来,词越多结果
/// 越多,与用户的直觉正好相反。
pub fn matches_all(query: &str, fields: &[&str]) -> bool {
    let toks = tokens(query);
    if toks.is_empty() {
        return true;
    }
    let lowered: Vec<String> = fields.iter().map(|f| f.to_lowercase()).collect();
    toks.into_iter()
        .all(|t| token_hits_lowered(&t.to_lowercase(), &lowered))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_lets_everything_through() {
        assert!(matches_all("", &["web01"]));
        assert!(matches_all("   ", &["web01"]));
    }

    /// 每个词各自可以命中**不同**的字段 —— 这正是分词的意义:用户记得住的
    /// 两件事往往分散在两个字段里(说明里写着爬虫、主机是 …219)。
    #[test]
    fn each_word_may_land_in_a_different_field() {
        assert!(matches_all("爬虫 219", &["爬虫采集", "10.0.2.219"]));
    }

    /// 词之间是 AND:少一个词命中就整行不出。
    ///
    /// **两个词里只让后一个落空**,前一个照常命中 —— 两个都落空的话,
    /// `.all` 换成 `.any` 这个变异照样返回 false,这条守护就杀不掉它。
    ///
    /// 自证会变红:把 `matches_all` 里的 `.all(` 换成 `.any(`。
    #[test]
    fn a_word_that_matches_nothing_rejects_the_whole_row() {
        assert!(!matches_all("爬虫 219", &["爬虫采集", "10.0.2.7"]));
    }

    /// 中文输入法敲出来的是**全角空格 U+3000**。按 `' '` 切的话这个查询会变成
    /// 一个词「爬虫　219」,谁都命中不了 —— 而且完全不报错。
    ///
    /// 自证会变红:把 `tokens` 换成 `query.split(' ').filter(|s| !s.is_empty())`。
    #[test]
    fn an_ideographic_space_separates_words_just_like_an_ascii_one() {
        assert_eq!(tokens("爬虫\u{3000}219"), vec!["爬虫", "219"]);
        assert!(matches_all("爬虫\u{3000}219", &["爬虫采集", "10.0.2.219"]));
    }

    #[test]
    fn matching_is_case_insensitive_on_both_sides() {
        assert!(matches_all("WEB", &["web01"]));
        assert!(matches_all("web", &["WEB01"]));
    }

    /// 一个词整串落在一个字段里就算命中 —— 词内部**不再**切分。
    /// 「01 在 name 里、web 在 host 里」不能拼出 `web01` 的命中。
    #[test]
    fn a_single_word_must_land_whole_inside_one_field() {
        assert!(!matches_all("web01", &["web", "01"]));
    }

    #[test]
    fn token_hits_answers_for_one_word_only() {
        assert!(token_hits("Web", &["web01"]));
        assert!(!token_hits("db", &["web01"]));
    }
}
