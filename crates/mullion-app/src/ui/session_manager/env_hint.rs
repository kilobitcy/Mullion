//! env 变量名的「这看着像在存密码」启发式(走查 18)。**纯函数,零 egui。**
//!
//! 走查 18 说 `ENV_WARNING` 常驻红框是噪音:天天见的警告等于没有警告,用户
//! 学会跳过它,真出事那次也跳过了。降成一行常驻灰字,**只在真的往变量名里
//! 写了 PASSWORD / TOKEN 这类词时**才升回红框。
//!
//! 取向是**宁可误报**:误报的代价是多看一行红字;漏报的代价是用户真的把密码
//! 明文存进了 `sessions.toml`,并 export 到远端 shell 历史里。所以
//! `SSH_KEY_PATH`(其实只是个路径)也会被点名 —— 这是有意的。

/// 整词匹配的词。太短,子串匹配会把 `KEYBOARD_LAYOUT`、`PASSENGER_COUNT`
/// 一起抓进来,那种误报密到会让人立刻学会无视整条提示。
const WHOLE_WORDS: &[&str] = &["KEY", "KEYS", "PW", "PASS", "AUTH"];

/// 子串匹配的词。这些词长到不会误伤,`MYPASSWORD`、`dbtoken` 这类不带
/// 分隔符的写法也得抓住。
const SUBSTRINGS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "TOKEN",
    "CREDENTIAL",
    "APIKEY",
    "PRIVATEKEY",
];

/// 这个变量名看着像在存密码吗?
pub(super) fn looks_like_secret(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    if SUBSTRINGS.iter().any(|w| upper.contains(w)) {
        return true;
    }
    // 按非字母数字切词:`AWS_SECRET-ACCESS.KEY` 的每一段都要能单独比。
    upper
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| WHOLE_WORDS.contains(&tok))
}

/// 命中时的红框文案。**点名是哪几个变量** —— 只说「有变量像密码」的话,
/// 一张十行的表用户得挨个看过去。
pub(super) fn secret_warning(keys: &[String]) -> String {
    format!(
        "「{}」看着像在存密码 —— 值以明文存进 sessions.toml(不进 secrets.enc),\
并会以 export 行发到远端,落进 shell 历史与 /proc/<pid>/environ。要存密码请用凭据。",
        keys.join("」「")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 日常变量名不能触发警告。误报密到这个程度,用户会学会无视整条提示,
    /// 真命中那次也一起无视了 —— 那就白做了。
    #[test]
    fn ordinary_names_stay_quiet() {
        for k in [
            "PATH",
            "LANG",
            "TERM",
            "EDITOR",
            "KEYBOARD_LAYOUT", // 含 KEY 但不是整词
            "PASSENGER_COUNT", // 含 PASS 但不是整词
            "HTTP_PROXY",
            "NODE_ENV",
        ] {
            assert!(!looks_like_secret(k), "{k} 不该被当成密码");
        }
    }

    /// 常见的存密码写法都要抓住。这是这个模块存在的全部理由。
    #[test]
    fn common_secret_names_are_caught() {
        for k in [
            "PASSWORD",
            "MY_PASSWORD",
            "DB_PASSWD",
            "PASSPHRASE",
            "API_KEY",
            "GITHUB_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "CREDENTIALS",
            "APIKEY",       // 无分隔符
            "dbtoken",      // 无分隔符 + 小写
            "PW",           // 整词
            "AUTH",         // 整词
            "SSH_KEY_PATH", // 有意的误报:宁可多报
        ] {
            assert!(looks_like_secret(k), "{k} 应该被点名");
        }
    }

    /// 大小写不敏感 —— 小写变量名在 shell 里同样常见。
    #[test]
    fn matching_is_case_insensitive() {
        assert!(looks_like_secret("my_password"));
        assert!(looks_like_secret("Api_Key"));
        assert_eq!(looks_like_secret("password"), looks_like_secret("PASSWORD"));
    }

    /// 红框必须点名是**哪几个**变量。一张十行的表,只说「有变量像密码」
    /// 用户得挨个看过去。
    #[test]
    fn the_warning_names_every_offending_key() {
        let msg = secret_warning(&["DB_PASSWORD".to_string(), "API_KEY".to_string()]);
        assert!(msg.contains("DB_PASSWORD"), "漏了第一个: {msg}");
        assert!(msg.contains("API_KEY"), "漏了第二个: {msg}");
        assert!(msg.contains("凭据"), "得给出替代做法: {msg}");
    }
}
