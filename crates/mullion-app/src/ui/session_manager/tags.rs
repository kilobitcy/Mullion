//! 标签的解析与去重(走查 6 / F63 欠账)。**纯函数,零 egui。**
//!
//! `identity.tags` 早就在 schema 里、已经被搜索命中、也已经参与分组的 Merge
//! 继承 —— 唯独没有编辑入口。搜索框的占位符却写着「搜索名称 / 主机 / 标签」,
//! 等于承诺了一个用户根本填不进去的东西。这里补的就是那个入口的判据部分。

/// 一个标签最多留多少个**字符**(不是字节)。列表和 chips 都只有一行的宽度,
/// 一条长句子当标签会把行撑爆。按字符截断:中文标签按字节切会 panic。
const MAX_LEN: usize = 32;

/// 把用户输入切成若干标签。
///
/// 分隔符取逗号(中英文)和空白 —— 用户从别处粘一串 `prod, web, cn` 或
/// `prod web cn` 进来都该正常工作。空白也算分隔符意味着**标签内不能有空格**;
/// 这是有意的取舍:允许空格的话,粘贴一整句备注进来会变成一个巨长的标签。
pub(super) fn parse(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input
        .split([',', '，', ';', '；'].as_ref())
        .flat_map(str::split_whitespace)
    {
        let tag: String = raw.trim().chars().take(MAX_LEN).collect();
        if tag.is_empty() {
            continue;
        }
        if !contains(&out, &tag) {
            out.push(tag);
        }
    }
    out
}

/// 已有列表里是否已经有这个标签。**大小写不敏感** —— 「Prod」和「prod」在
/// 用户心里是同一个标签,分成两条只会让搜索和继承都少命中一半。
pub(super) fn contains(tags: &[String], tag: &str) -> bool {
    tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
}

/// 把一次输入并进已有标签列表,返回是否真的加进去了东西。
///
/// 保序追加、跳过已有 —— 重排既有标签会让用户刚看清的那一排 chips 跳位置。
pub(super) fn merge_into(tags: &mut Vec<String>, input: &str) -> bool {
    let mut added = false;
    for tag in parse(input) {
        if !contains(tags, &tag) {
            tags.push(tag);
            added = true;
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_is_split_on_commas_and_whitespace_then_trimmed() {
        assert_eq!(parse("prod, web  cn"), vec!["prod", "web", "cn"]);
        assert_eq!(parse("  "), Vec::<String>::new());
        assert_eq!(parse(",,,"), Vec::<String>::new());
        // 中文逗号也认 —— 中文输入法下打出来的就是这个。
        assert_eq!(parse("生产,测试"), vec!["生产", "测试"]);
    }

    /// 大小写不敏感去重:「Prod」和「prod」是同一个标签,分成两条会让搜索
    /// 和分组继承各少命中一半。保留**先出现**的那个拼法。
    #[test]
    fn duplicates_are_dropped_case_insensitively_keeping_the_first_spelling() {
        assert_eq!(parse("Prod prod PROD"), vec!["Prod"]);
        let mut tags = vec!["prod".to_string()];
        assert!(!merge_into(&mut tags, "PROD"), "已有的不该再加一条");
        assert_eq!(tags, vec!["prod"]);
        assert!(merge_into(&mut tags, "web"));
        assert_eq!(tags, vec!["prod", "web"], "新标签追加在末尾,不重排已有的");
    }

    /// 超长标签按**字符**截断。中文按字节切会当场 panic。
    #[test]
    fn an_overlong_multibyte_tag_is_truncated_on_char_boundaries() {
        let long = "生产环境".repeat(20);
        let got = parse(&long);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].chars().count(), MAX_LEN);
        assert!(long.starts_with(&got[0]));
    }
}
