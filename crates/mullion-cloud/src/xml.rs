//! ListObjectsV2 的响应里抠出 `<Key>` 与 `<NextContinuationToken>`(F270)。
//!
//! **刻意不引 XML crate**:我们只需要两个标签的文本,而每多一个依赖就多一份
//! exe 体积与交叉编译风险(理由同根 Cargo.toml 里不用 `image` 那几条)。
//!
//! 代价写清楚:这是**标签扫描,不是 XML 解析**。键里如果出现 `&lt;` 这类
//! 实体,我们要反转义;CDATA、命名空间前缀、注释一律不认。对象键由我们自己
//! 生成(纯数字与短横),这些情况不会出现 —— 但**别拿这个函数去解别的 XML**。

use crate::error::CloudError;

/// 一次 ListObjectsV2 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListResult {
    pub keys: Vec<String>,
    /// `Some` = 还有下一页,把它当 `continuation-token` 再发一次。
    pub next_token: Option<String>,
}

/// 抠出 `<tag>` 的全部文本。标签名精确匹配(带上尖括号),避免前缀撞名。
fn take_all(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        let after = &rest[i + open.len()..];
        let Some(j) = after.find(&close) else { break };
        out.push(unescape(&after[..j]));
        rest = &after[j + close.len()..];
    }
    out
}

/// XML 的五个预定义实体。顺序要紧:`&amp;` **必须最后换**,否则
/// `&amp;lt;` 会先变成 `&lt;` 再变成 `<` —— 静默多解一层。
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 解析 ListObjectsV2 的响应体。
pub fn parse_list(xml: &str) -> Result<ListResult, CloudError> {
    if !xml.contains("<ListBucketResult") {
        return Err(CloudError::Malformed(format!(
            "不是 ListBucketResult:{}",
            xml.chars().take(200).collect::<String>()
        )));
    }
    Ok(ListResult {
        keys: take_all(xml, "Key"),
        next_token: take_all(xml, "NextContinuationToken").into_iter().next(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>my-bucket</Name>
  <Prefix>mullion/</Prefix>
  <IsTruncated>false</IsTruncated>
  <Contents><Key>mullion/000001-20260915T101500Z.mpk</Key><Size>4096</Size></Contents>
  <Contents><Key>mullion/000002-20260915T111500Z.mpk</Key><Size>4100</Size></Contents>
</ListBucketResult>"#;

    #[test]
    fn every_key_is_extracted_in_order() {
        let r = parse_list(SAMPLE).expect("解析失败");
        assert_eq!(
            r.keys,
            vec![
                "mullion/000001-20260915T101500Z.mpk".to_string(),
                "mullion/000002-20260915T111500Z.mpk".to_string(),
            ]
        );
        assert_eq!(r.next_token, None);
    }

    /// 截断标记与续页令牌必须一起读出来。**只读 keys 不读令牌**的后果:
    /// 超过 1000 个对象时我们只看得见第一页,而 ListObjectsV2 按字典序返回,
    /// 第一页里的最大序号**不是**真正的最大序号 —— 下一次上传会撞上一个
    /// 已存在的键,ForbidOverwrite 把它挡掉,表现是「备份莫名其妙失败」。
    #[test]
    fn a_truncated_response_yields_its_continuation_token() {
        let xml = "<ListBucketResult><IsTruncated>true</IsTruncated>\
                   <NextContinuationToken>abc123</NextContinuationToken>\
                   <Contents><Key>k1</Key></Contents></ListBucketResult>";
        let r = parse_list(xml).expect("解析失败");
        assert_eq!(r.next_token.as_deref(), Some("abc123"));
    }

    /// `<Name>` 与 `<Prefix>` 也是文本标签,但它们不是 `<Key>`。这条钉住的是
    /// 「有人把匹配写成 `contains("Key")`」——那会把 `<NextContinuationToken>`
    /// 也匹配进来(它的名字里就有 "Token" 没有 "Key",但 `<ETag>` 之类将来加的
    /// 标签就不一定了)。
    ///
    /// **这是构造式覆盖,不是门控,变异表里没有它是有意的。** 在当前实现
    /// (`<Key>` 带尖括号精确匹配)下它必然通过 —— 它防的是**将来有人换一种
    /// 匹配方式**,那时它才第一次有机会变红。不要为了「让它能被杀掉」去改
    /// 实现或删掉它。
    #[test]
    fn only_key_elements_are_taken_not_every_text_node() {
        let r = parse_list(SAMPLE).expect("解析失败");
        assert!(
            !r.keys.iter().any(|k| k == "my-bucket" || k == "mullion/"),
            "把 Name/Prefix 也当成 Key 了:{:?}",
            r.keys
        );
    }

    /// 第二条断言是**替换顺序**的守护,不是凑数:输入 `a&amp;lt;b` 的正解是
    /// `a&lt;b`(只解一层)。若把 `&amp;` 那次替换挪到最前面,会先得到
    /// `a&lt;b` 再被下一次替换解成 `a<b` —— 多解了一层。**只用 `a&amp;b`
    /// 做输入的话这个变异杀不掉**(两种顺序都得 `a&b`),那条守护就是恒绿的。
    #[test]
    fn xml_entities_in_a_key_are_unescaped() {
        let xml = "<ListBucketResult><Contents><Key>a&amp;b</Key></Contents></ListBucketResult>";
        assert_eq!(parse_list(xml).unwrap().keys, vec!["a&b".to_string()]);

        let nested =
            "<ListBucketResult><Contents><Key>a&amp;lt;b</Key></Contents></ListBucketResult>";
        assert_eq!(
            parse_list(nested).unwrap().keys,
            vec!["a&lt;b".to_string()],
            "实体只该解一层 —— `&amp;` 必须最后换"
        );
    }

    /// 服务端回了一段我们读不懂的东西时,**报错而不是返回空列表**。
    /// 返回空的话调用方会把序号从 1 重新开始,`ForbidOverwrite` 挡住之后
    /// 表现成「备份失败」,而真实原因(响应格式不对)被彻底吃掉。
    #[test]
    fn a_response_that_is_not_a_list_result_is_an_error_not_an_empty_list() {
        let r = parse_list("<Error><Code>AccessDenied</Code></Error>");
        assert!(r.is_err(), "非 ListBucketResult 必须报错,不能静默返回空列表");
    }
}
