//! 拖出时的**落地文件名**(F59 / 设计 N3)。零平台代码 —— 这是这一片里
//! 少数能在无头环境真正验证的部分。
//!
//! 远端名是字节真源,可能含 Windows 建不出文件的字符、以尾随点/空格结尾、
//! 或恰好撞上设备名。净化之后**两个不同的远端名可能撞成同一个 Windows 名**
//! (`a:b` 和 `a?b` 都变 `a_b`),资源管理器会拿后落地的那个盖掉前一个 ——
//! 用户看到的是「拖了 3 个下来只剩 2 个」,且没有任何报错。

/// Windows 文件名里建不出来的字符。反斜杠与斜杠也在内 —— 远端(POSIX)的名字
/// 里 `\` 完全合法,拖到 Windows 上就成了路径分隔符。
const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// MS-DOS 时代留下的设备名。**任何扩展名都算**(`NUL.txt` 一样建不出来),
/// 判据是「点之前那一段」。
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// 一个远端名 → 一个 Windows 上建得出来的名字。
///
/// 净化之后可能与同批里另一个撞名,**必须再过一遍 [`unique`]**。
pub fn sanitize(name: &str) -> String {
    // 控制字符(含 `\0`)一并换掉:它们在 `CreateFile` 上直接失败,而远端
    // 文件名里塞控制字符是真事(脚本生成的名字、以及故意的)。
    let mut s: String = name
        .chars()
        .map(|c| {
            if ILLEGAL.contains(&c) || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Windows 会**静默**丢掉尾随的点和空格(`report. ` 落地成 `report`),
    // 于是「我拖下来的名字和远端不一样」变成一个查无可查的现象。自己先剪掉,
    // 至少两边看到的是同一个名字。
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    if s.is_empty() {
        // 名字整个被剪没了(全是点/空格/非法字符)。给个占位,总好过一条
        // 必然失败的落地。
        return "_".to_string();
    }
    let stem = s.split('.').next().unwrap_or(&s);
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        // 加前缀而不是改名:用户还认得出这是哪个文件。
        return format!("_{s}");
    }
    s
}

/// 同一批拖出里去重。撞名的第二个起加 ` (2)`、` (3)`……(与资源管理器
/// 自己的重名规则同款,用户见过这个样子)。
///
/// 顺序稳定:入参顺序就是 `FILEGROUPDESCRIPTORW` 里的顺序,而
/// `CFSTR_FILECONTENTS` 用 `lindex` 按下标取流 —— 这里重排一次,
/// 目标程序拿到的就是张冠李戴的内容。
pub fn unique(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(names.len());
    for n in names {
        if !out.contains(n) {
            out.push(n.clone());
            continue;
        }
        // 从 2 开始试,直到不撞。`(2)` 本身也可能已被占用(远端真有一个
        // 叫 `a_b (2)` 的文件),所以要一路试下去而不是只试一次。
        let (stem, ext) = split_ext(n);
        let mut i = 2usize;
        loop {
            let cand = format!("{stem} ({i}){ext}");
            if !out.contains(&cand) {
                out.push(cand);
                break;
            }
            i += 1;
        }
    }
    out
}

/// `("报告", ".tar.gz")` 这种切法 —— 只切**最后**一个点,与资源管理器
/// 的重名规则一致(`报告.tar (2).gz` 才是错的)。开头就是点的(`.bashrc`)
/// 整个当主干,不然会变成 ` (2).bashrc`。
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn illegal_windows_characters_become_underscores() {
        assert_eq!(sanitize("a:b*c?.txt"), "a_b_c_.txt");
        // 远端(POSIX)的名字里反斜杠完全合法,拖到 Windows 上就是路径分隔符。
        assert_eq!(sanitize(r"a\b"), "a_b");
    }

    #[test]
    fn control_characters_are_replaced_because_createfile_rejects_them() {
        assert_eq!(sanitize("a\u{1}b\tc"), "a_b_c");
    }

    #[test]
    fn a_reserved_device_name_gets_a_prefix_so_the_file_can_exist_at_all() {
        // Windows 上这些名字建不出文件,资源管理器会静默失败 ——
        // 用户看到的是「拖下来少了一个」,没有任何报错。
        assert_eq!(sanitize("NUL"), "_NUL");
        assert_eq!(sanitize("com1.txt"), "_com1.txt", "带扩展名照样是设备名");
        assert_eq!(
            sanitize("common.txt"),
            "common.txt",
            "只有恰好是设备名才加前缀"
        );
    }

    #[test]
    fn trailing_dots_and_spaces_are_stripped_because_windows_silently_drops_them() {
        // 不自己剪的话,落地名与远端名不一致,而且没有任何提示。
        assert_eq!(sanitize("report. "), "report");
        assert_eq!(sanitize("report..."), "report");
    }

    #[test]
    fn a_name_that_sanitizes_to_nothing_still_gets_a_usable_placeholder() {
        assert_eq!(sanitize("..."), "_");
        assert_eq!(sanitize(""), "_");
    }

    #[test]
    fn two_different_remote_names_that_sanitize_alike_do_not_overwrite_each_other() {
        // `a:b` 与 `a?b` 净化后都是 `a_b`。不去重的话资源管理器会拿后一个
        // 盖掉前一个,用户「拖了 3 个下来只剩 1 个」。
        let out = unique(&["a_b".into(), "a_b".into(), "a_b".into()]);
        assert_eq!(out, vec!["a_b", "a_b (2)", "a_b (3)"]);
    }

    #[test]
    fn the_dedup_suffix_goes_before_the_extension_like_the_explorer_does() {
        let out = unique(&["报告.txt".into(), "报告.txt".into()]);
        assert_eq!(out, vec!["报告.txt", "报告 (2).txt"]);
    }

    #[test]
    fn a_dotfile_keeps_its_leading_dot_as_part_of_the_stem() {
        // `.bashrc` 切成 `("", ".bashrc")` 的话,重名会变成 ` (2).bashrc`。
        let out = unique(&[".bashrc".into(), ".bashrc".into()]);
        assert_eq!(out, vec![".bashrc", ".bashrc (2)"]);
    }

    #[test]
    fn dedup_skips_a_suffix_that_the_remote_already_uses() {
        // 远端真有一个叫 `a (2).txt` 的文件时,不能把撞名的那个也叫这个名。
        let out = unique(&["a.txt".into(), "a (2).txt".into(), "a.txt".into()]);
        assert_eq!(out, vec!["a.txt", "a (2).txt", "a (3).txt"]);
    }

    #[test]
    fn the_output_order_matches_the_input_order() {
        // `CFSTR_FILECONTENTS` 按 `lindex`(下标)取流。这里重排一次,
        // 目标程序拿到的就是张冠李戴的内容。
        let out = unique(&["c".into(), "a".into(), "b".into()]);
        assert_eq!(out, vec!["c", "a", "b"]);
    }
}
