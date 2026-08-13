//! F21:字体族清单的整理与等宽校验。**纯函数,零 GPU、零 fontdb 类型**。
//!
//! 真正去问系统装了哪些字体的是 `text.rs`(它握着 `FontSystem`);这里只收
//! 一堆 `(族名, 是不是等宽)` 然后决定「怎么排、怎么判」——把判断从 GPU 胶水
//! 里抠出来,是本项目唯一能给这类逻辑写测试的办法。

/// 下拉里的一条候选字体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontChoice {
    /// 字体族名,原样交给 cosmic-text 的 `Family::Name`。
    pub name: String,
    /// 字体自己在 `post` 表里声明的等宽标志。**不是我们量出来的**——
    /// 有的等宽字体没置这一位,所以它只用来排序和打标,不用来过滤
    /// (真正的判据是 [`is_monospace_advance`],在用户选中之后当场量)。
    pub monospaced: bool,
}

/// 去重 + 排序:等宽的排前面,组内按族名。
///
/// **去重是必须的**:fontdb 的 `faces()` 一个字重一条,「同一款字体」会以
/// Regular/Bold/Italic… 的形式出现四五次,不去重的话下拉里全是重复项。
/// 同名条目只要**有任意一个字重声明了等宽**就算等宽 —— 部分字族只在
/// Regular 上置了这一位。
///
/// **不过滤非等宽**:`monospaced` 来自字体自己的 `post` 表,漏置的不少,
/// 一律过滤掉会让用户在列表里找不到他刚装的那款等宽字体。
pub fn sort_families(raw: Vec<(String, bool)>) -> Vec<FontChoice> {
    let mut out: Vec<FontChoice> = Vec::new();
    for (name, mono) in raw {
        if name.trim().is_empty() {
            continue;
        }
        match out.iter_mut().find(|c| c.name == name) {
            Some(c) => c.monospaced |= mono,
            None => out.push(FontChoice {
                name,
                monospaced: mono,
            }),
        }
    }
    // 等宽在前(`!monospaced` 升序 = false 在前),组内按族名。
    // `to_lowercase` 排序键:大小写混排会让 "iosevka" 排到 "Zed Mono" 后面。
    out.sort_by(|a, b| {
        (!a.monospaced, a.name.to_lowercase()).cmp(&(!b.monospaced, b.name.to_lowercase()))
    });
    out
}

/// 两个字符的 advance 够不够接近,算不算等宽。
///
/// 容差 1%:hinting 会让同一款等宽字体的不同字形差出零点几个像素,要求
/// 严格相等会把真等宽字体也判成非等宽。
///
/// 判据用 `M` 与 `i` 而不是 `M` 与 `W`:比例字体里 `M`/`W` 本来就都很宽,
/// 差别不明显;`i` 是最窄的那一类,差异最大。
pub fn is_monospace_advance(m: f32, i: f32) -> bool {
    if !m.is_finite() || !i.is_finite() || m <= 0.0 {
        // 量不出来就**不报警**:一个量不到宽度的字体多半根本没加载成功,
        // 这时候弹「不是等宽字体」是指错了方向(真正的问题是 `family_missing`)。
        return true;
    }
    (m - i).abs() <= m * 0.01
}

/// 用户填的字体族在不在系统里。
///
/// 大小写不敏感:fontdb 报的族名大小写跟用户手打的、跟字体文件里写的
/// 三者常常不一致,大小写敏感比对会把装着的字体判成「没装」。
///
/// **这条判断存在的全部理由**:cosmic-text 匹配不到族名时会静默回退到默认
/// 字体——画面看着完全正常,用户只会以为设置没生效。
pub fn family_missing(chosen: &str, known: &[FontChoice]) -> bool {
    let want = chosen.trim();
    if want.is_empty() {
        // 空 = 用内置默认,不算「缺失」。
        return false;
    }
    !known.iter().any(|c| c.name.eq_ignore_ascii_case(want))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[FontChoice]) -> Vec<&str> {
        v.iter().map(|c| c.name.as_str()).collect()
    }

    /// fontdb 一个字重一条,同族会重复出现好几次。不去重的话下拉里全是重复项。
    ///
    /// 自证会变红:把 `sort_families` 里那段 `find(|c| c.name == name)` 换成
    /// 无条件 `push`。
    #[test]
    fn the_same_family_listed_once_per_weight_collapses_into_one_row() {
        let out = sort_families(vec![
            ("Cascadia Mono".into(), true),
            ("Cascadia Mono".into(), true),
            ("Cascadia Mono".into(), true),
        ]);
        assert_eq!(names(&out), vec!["Cascadia Mono"]);
    }

    /// 只有部分字重置了等宽位时,整族仍算等宽 —— 否则同一款字体会因为
    /// 扫到的字重顺序不同而时前时后。
    #[test]
    fn a_family_counts_as_monospaced_if_any_weight_says_so() {
        let out = sort_families(vec![("Iosevka".into(), false), ("Iosevka".into(), true)]);
        assert_eq!(out.len(), 1);
        assert!(out[0].monospaced);
    }

    /// 等宽排前面,组内按族名(大小写不敏感)。终端里选到比例字体会整屏错列,
    /// 把等宽的顶上去是这份列表的主要价值。
    ///
    /// 自证会变红:把排序键里的 `!a.monospaced` 去掉。
    #[test]
    fn monospaced_families_come_first_then_alphabetical() {
        let out = sort_families(vec![
            ("Arial".into(), false),
            ("zed mono".into(), true),
            ("Iosevka".into(), true),
            ("Times".into(), false),
        ]);
        assert_eq!(names(&out), vec!["Iosevka", "zed mono", "Arial", "Times"]);
    }

    /// 空族名不进列表 —— fontdb 偶尔会报出空名条目,进了列表就是一条点不动
    /// 的空行。
    #[test]
    fn a_blank_family_name_never_makes_it_into_the_list() {
        assert!(sort_families(vec![("".into(), true), ("   ".into(), false)]).is_empty());
    }

    /// 1% 容差:hinting 会让同款等宽字体的不同字形差出零点几像素。
    ///
    /// 自证会变红:把容差改成 `== 0.0` 或放宽到 50%。
    #[test]
    fn a_hairline_difference_still_counts_as_monospace_but_a_real_one_does_not() {
        assert!(is_monospace_advance(10.0, 10.0));
        assert!(
            is_monospace_advance(10.0, 10.05),
            "0.5% 差异属于 hinting 噪声"
        );
        assert!(!is_monospace_advance(10.0, 4.0), "比例字体必须被认出来");
    }

    /// 量不出宽度时**不报警**:那说明字体压根没加载成功,该报的是
    /// 「找不到这个字体」,不是「它不是等宽的」——指错方向的提示比没有提示更糟。
    #[test]
    fn an_unmeasurable_font_does_not_trigger_the_monospace_warning() {
        assert!(is_monospace_advance(0.0, 0.0));
        assert!(is_monospace_advance(f32::NAN, 5.0));
    }

    /// 大小写不敏感:fontdb 报的族名与用户手打的常常大小写不一致,敏感比对
    /// 会把装着的字体判成「没装」。
    ///
    /// 自证会变红:把 `eq_ignore_ascii_case` 换成 `==`。
    #[test]
    fn a_font_that_is_installed_is_not_reported_missing_over_letter_case() {
        let known = sort_families(vec![("Cascadia Mono".into(), true)]);
        assert!(!family_missing("cascadia mono", &known));
        assert!(
            !family_missing("  Cascadia Mono  ", &known),
            "两端空白不算数"
        );
        assert!(family_missing("Comic Sans MS", &known));
    }

    /// 留空 = 用内置默认,不是「缺失」。
    #[test]
    fn leaving_the_family_blank_is_not_a_missing_font() {
        assert!(!family_missing("", &[]));
        assert!(!family_missing("   ", &[]));
    }
}
